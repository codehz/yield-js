# 直接环境访问对照

本页测的是绑定访问的两步直接化：模块自身绑定直接寻址模块环境，以及无每轮环境、闭包创建深度唯一时用固定 `(depth, binding)` 读写。实现见 `compiler/src/lower/mod.rs` 的 `direct_bindings` / `function_depths` 与 `vm/src/heap/store.rs`。原始数据在本目录。

## 复现

基线为提交 `1617305`，优化工作树与基线使用同一 `Cargo.lock`、release bench profile 和夹具。计时用独立进程：

```sh
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench -p yield-harness --bench baseline -- --samples 5 > base.csv
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench -p yield-harness --bench baseline -- --samples 5 > after-a.csv
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench -p yield-harness --bench baseline -- --samples 5 > after-b.csv
YIELD_VM_BENCH_DIAGNOSTICS=1 cargo bench -p yield-harness --bench baseline -- --samples 1 > base-counters.csv
YIELD_VM_BENCH_DIAGNOSTICS=1 cargo bench -p yield-harness --bench baseline -- --samples 1 > after-counters.csv
```

- `base.csv` 是改动前一批，`after-a.csv` / `after-b.csv` 是改动后两批；每批预热 2 次、保留 5 个样本，表中为各批中位数。
- 诊断计数单独编译运行，只用于解释分派与查找次数，不用诊断构建计时。
- 挂起负载各自用本构建的产物恢复；计时仍校验首次执行、稳态结果和恢复续跑。

## 时间

单位 µs，`entry_steady_5` 已除以 5。负值为耗时下降。

| 工作负载 | 首次：基线 / A / B | A / B 变化 | 稳态：基线 / A / B | A / B 变化 |
| --- | ---: | ---: | ---: | ---: |
| React 首提交 | 477.38 / 477.17 / 465.58 | −0.0% / −2.5% | 429.31 / 414.25 / 408.07 | −3.5% / −4.9% |
| React 更新 | 2727.00 / 2620.42 / 2590.12 | −3.9% / −5.0% | 2175.88 / 2070.67 / 2068.08 | −4.8% / −5.0% |
| 短调用 | 286.79 / 248.96 / 265.83 | −13.2% / −7.3% | 290.43 / 258.02 / 257.26 | −11.2% / −11.4% |
| 数值/分支 | 962.96 / 792.00 / 791.54 | −17.8% / −17.8% | 969.39 / 790.80 / 794.29 | −18.4% / −18.1% |
| 闭包读写 | 413.38 / 403.04 / 387.33 | −2.5% / −6.3% | 435.24 / 412.92 / 414.67 | −5.1% / −4.7% |
| 静态属性 | 361.67 / 361.33 / 362.25 | −0.1% / +0.2% | 342.02 / 340.18 / 343.08 | −0.5% / +0.3% |

React 的 `startup_initialize` 从 1552.33 / 1562.58 µs 降到 A 批 1518.21 / 1525.96、B 批 1522.17 / 1501.42 µs（约 −2% 到 −4%）。短调用、数值/分支和闭包负载的收益稳定超过两批基线波动；静态属性负载没有绑定查找，保持持平。

## 诊断计数

同源输入的动态计数。`binding_lookups` 统计查找 owner 时访问的环境数，不包含直接加载或最终命中环境的读取。

| 工作负载 | 阶段 | 基线 | 优化后 |
| --- | --- | ---: | ---: |
| React 首提交 | 启动 | 2,853 | 670 |
| React 首提交 | 首次 / 稳态 5 次 | 1,358 / 6,780 | 0 / 0 |
| React 更新 | 首次 / 稳态 5 次 | 11,846 / 52,410 | 0 / 0 |
| 短调用 | 首次 / 稳态 5 次 | 2,048 / 10,240 | 0 / 0 |
| 数值/分支 | 首次 / 稳态 5 次 | 8,704 / 43,520 | 0 / 0 |
| 闭包读写 | 首次 / 稳态 5 次 | 4,100 / 20,500 | 0 / 0 |

动态分派次数不变，说明收益来自移除环境链遍历，而不是少执行指令。React 启动仍有 670 次遍历，落在带每轮环境或闭包创建深度不一致的函数上；这些路径保留 `GetBinding` / `SetBinding` 回退。

## 代价

React 产物从 802,607 B 增到 814,499 B（+1.5%），产物解码中位数从 747.29 µs 增到 A 批 787.50、B 批 769.33 µs。新增 direct opcode 带 `depth` 操作数；产物仍使用原有 `BUILD_FORMAT_VERSION` 和 bincode 编码。该解码代价小于本轮首次/稳态执行的收益，但冷加载单独评估时不能忽略。

字节码版本升为 **48**；快照记录形状未变，`SNAPSHOT_FORMAT_VERSION` 不升。旧产物和旧快照必须由兼容版本的 VM 使用；`build_id` 随指令内容变化。

## 验证

`cargo fmt --check`、`cargo clippy --workspace --all-targets`、`cargo test --workspace` 均通过。测试覆盖闭包、循环每轮绑定、类作用域、生成器、`await` 恢复和快照；跨闭包提前读取仍为 `undefined`，模块链接读取的 TDZ 行为未改。

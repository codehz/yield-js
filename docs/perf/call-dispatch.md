# 脚本调用分派：复用 classify 结果

上一轮把函数 ID 查找改成密集索引后, 每次脚本调用仍会在 `invoke` 里重新读取函数对象:

```text
classify(callee) -> Dispatch::Script { func_id, env }
invoke             -> heap.function(id) 查 class
                   -> heap.function(id) 查 home/callee
call_function      -> function_index
```

本轮让 `Dispatch::Script` 在 `classify` 内一次取出并携带 `callee`、`home` 和 `class` 标记. `invoke` 不再为每次脚本调用重复访问堆对象, 直接进入 `call_function`; 稀疏函数 ID 仍走上一轮的排序向量回退. 不改变字节码、快照或构建摘要.

## 环境与命令

Apple M2 (24 GiB), macOS 27.0.1, rustc/cargo 1.99.1. 基线是提交 `93f9668`（函数索引已启用）; 最终版是同一工作树上的改动. 两版各跑 A/B/C 三批, 每批 `--samples 5`, 进程内预热 2 次; 报告使用每批中位数. 复现:

```sh
cargo bench -p yield-harness --bench baseline -- --samples 5
```

基线原始样本复用 [函数索引对照](../function-index/lookup-a/b/c.csv); 本轮样本在 [call-dispatch/](call-dispatch/) 的 `dispatch-a/b/c.csv`.

## 结果

| 负载 / 指标 | 基线 (µs) | 最终版 (µs) | 变化 |
|---|---:|---:|---:|
| React 首提交 / `entry_first` | 472.79 | 465.29 | −1.6% |
| React 首提交 / `entry_steady_5` | 2064.50 | 2022.00 | −2.1% |
| React 更新 / `entry_first` | 2589.58 | 2512.12 | −3.0% |
| React 更新 / `entry_steady_5` | 10162.96 | 9908.00 | −2.5% |
| 短调用 / `entry_first` | 250.50 | 240.17 | −4.1% |
| 短调用 / `entry_steady_5` | 1273.79 | 1207.92 | −5.2% |
| 闭包读写 / `entry_steady_5` | 2052.88 | 1982.33 | −3.4% |
| 静态属性 / `entry_steady_5` | 1241.46 | 1229.75 | −0.9% |

短调用、闭包读写和 React 两个负载在三批中方向一致; 静态属性只有约 1% 的稳态收益. 数值/分支稳态中位数 +1.5%, 异步生成器稳态 +3.5%, 但对应的 `entry_first` 分别为 +0.3% / −2.7%, 且数值负载不使用脚本调用分派, 按样本波动处理, 不据此宣称收益. `startup_initialize` 在短调用/闭包上出现 0.1–0.2 µs 的抖动, 不影响端到端.

## 未启用项

本轮没有改动帧分配. 上一轮已测得帧缓冲复用和小槽内联均未达到启用门槛, 数据保留在 [函数索引对照](../function-index.md) 的 `frame-pool-*.csv` 与 `inline-slots-*.csv`.

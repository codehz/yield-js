# 字节码加载与恢复：现状数据

产物与摘要历经 JSON → SHA-256 → xxh3-128 → 二进制摘要的演进；本轮只保留现状（二进制产物 + xxh3-128 + bincode 容器）的数据与验证结论，历史实验（SHA-256、xxh3+JSON、JSON 产物）的原始 CSV 已删除。方法：同一基准、独立 release 进程、每批预热 2 次、样本 9–15，校验首次执行、稳态结果与快照续跑。复现入口见[性能基线](performance-baseline.md#复现)。

## 启动准备分项

计时点在 `Vm::new_profiled` 的启动路径读取；普通构造和恢复不采集。逐样本核对三个子项之和等于 `startup_prepare`。React 首提交中位数：

| 阶段 | A | B |
| --- | ---: | ---: |
| 产物解码 | 15.39 ms | 16.17 ms |
| 启动准备合计 | 17.17 ms | 17.63 ms |
| 　构建身份校验（当时含 JSON 序列化 + SHA-256） | 15.90 ms | 16.28 ms |
| 　字节码结构校验 | 0.90 ms | 0.97 ms |
| 　宿主链接及 VM 准备 | 0.35 ms | 0.37 ms |
| 模块初始化 | 1.58 ms | 1.65 ms |

该批采于摘要优化之前，仅用于展示启动阶段的构成比例，不能外推到当前实现。

## 二进制产物

`Build::to_bytes` / `Build::from_bytes`：8 字节 `YIELDBLD` magic、little-endian `BUILD_FORMAT_VERSION`、bincode fixed-int little-endian payload。`Instr` 为 externally tagged；`JsString` 与数字按 `is_human_readable()` 在 JSON 和二进制表示间分支，二进制路径不用 untagged/内部 tag。React 首提交中位数（样本范围见 [artifact-*.csv](bytecode-loading/)）：

| 指标 | JSON artifact | 二进制 artifact |
| --- | ---: | ---: |
| 产物大小 | 4,140,678 B | 802,607 B |
| 产物编码 | 4.34 ms | 0.55–0.56 ms |
| 产物解码 | 15.32–15.35 ms | 0.75–0.76 ms |
| 启动准备 | 1.90 ms | 1.77 ms |
| 模块初始化 | 1.57 ms | 1.56–1.57 ms |
| 加载合计 | 18.80–18.84 ms | 4.09 ms |

产物缩小约 80.6%，解码约 20 倍，首提交加载合计约 4.6 倍。`build_id` 因二进制表示再次变化；产物格式由 `BUILD_FORMAT_VERSION` 独立约束。当前基线：React 构建约 0.75 MB，构建身份校验约 0.55 ms。

## 函数引用校验索引（未启用）

以 `BTreeMap` 取代 `Build::validate` 的线性函数引用扫描：React 启动准备两批约 −1.1%，但样本范围重叠，未改动的解码也有同量级波动，四个挂起负载的 `restore` 未见稳定改善。未达到启用门槛，未合入。样本 `base-*.csv`、`indexed-*.csv` 与补丁 `indexed-validation.patch` 保留用于复现。

## 复用已验证的构建

`VerifiedBuild::new` 完整校验 `Build` 并派生函数索引和调用元数据；`Vm::new_verified` / `Vm::restore_verified` 共享不可变的线程内句柄，不共享堆、缓存或宿主状态。普通入口仍每次校验；复用入口不省略新 VM 的宿主链接、模块初始化或快照校验。

```sh
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline -- --verified-reuse --samples 11
```

三批独立进程，每样本顺序启动 8 个新 VM 或恢复同一镜像 8 次，交错比较两条入口；同一二进制与产物，每个 VM 新宿主，验证运行结果与续跑。[原始 CSV](bytecode-loading/verified-reuse/)。

| 工作负载 | 每次校验，8 次总耗时 | 校验一次 + 复用 | 变化 |
| --- | ---: | ---: | ---: |
| React 启动 | 24.50–24.62 ms | 14.65–14.67 ms | −40.3% |
| React 恢复 | 28.85–28.95 ms | 19.23–19.24 ms | −33.3% 至 −33.5% |
| `await-success` 启动 | 2.12–2.15 ms | 2.09–2.11 ms | −0.7% 至 −2.2% |
| `await-success` 恢复 | 3.84–3.87 ms | 3.89–3.92 ms | +0.5% 至 +1.7% |

React 构建约 752 KB，一次校验和派生约 1.42 ms；调用方已持有句柄时 8 次启动/恢复分别省约 46% 和 38%。`await-success` 构建仅 2.7 KB（校验约 5–6 µs），恢复仍主要花在堆与快照检查上。不代表单 VM 冷加载收益。

## 二进制快照

`Snapshot::to_bytes` / `from_bytes` 使用 `YIELDSNP`、独立容器版本和镜像记录版本，后接 bincode 小端变长整数编码；JSON 保持原样，两者解码到相同记录并执行同一恢复校验。

```sh
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline -- --snapshot-codecs --samples 15
```

三批独立进程，15 个配对样本，交替两格式顺序；对同一捕获记录分别计编码、解码和 `Vm::restore_verified`，已验证构建计时外共享。[原始 CSV](bytecode-loading/snapshot-codecs/)。

| 工作负载 | JSON → 二进制（字节） | 编码降幅 | 解码降幅 | 解码 + 恢复降幅 |
| --- | ---: | ---: | ---: | ---: |
| React 首提交后 | 1,055,295 → 161,557 | 71.2–71.7% | 60.0–60.4% | 30.1–30.6% |
| `await` 成功 | 302,611 → 46,600 | 70.7–70.9% | 57.2–57.7% | 32.8–33.5% |
| `finally` 中 `await` 拒绝 | 303,149 → 46,684 | 70.3–70.9% | 56.8–57.2% | 32.9–33.7% |
| 同步生成器 | 305,015 → 46,940 | 70.2–70.9% | 56.9–57.8% | 33.0–33.4% |
| 异步生成器 | 305,015 → 46,940 | 70.1–71.1% | 57.2–57.5% | 32.9–33.6% |

单独的恢复阶段没有稳定差异（−1.8% 到 +1.4%）；收益来自编解码，不来自省略堆还原或校验。

## 恢复阶段拆分

```sh
env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline -- --restore-profile --samples 15
```

对同一镜像和已验证构建交替跑带计时点入口和普通恢复入口；二进制解码、构建校验、宿主重挂和续跑不计入恢复。各阶段占比先按样本计算再取中位数。[原始 CSV](bytecode-loading/restore-profile/)。

| 阶段 | React 首提交后（2,195 对象，2.19–2.21 ms） | 四个挂起负载（517–524 对象，0.47–0.48 ms） |
| --- | ---: | ---: |
| 兼容性检查与解析导入 | <0.1% | 0.2–0.3% |
| 堆对象插入（含局部检查和 RegExp 重建） | 9.1–9.6% | 9.2–9.5% |
| 生成器、Symbol、私有元素等侧表加载与检查 | 约 0.5% | 0.5–0.6% |
| 原生目标与身份检查 | 约 4.1% | 7.3–7.6% |
| 根、模块、续体、宿主记录加载与检查 | 4.5–4.6% | 9.7–10.2% |
| VM 组装和导入安装 | 1.5–1.6% | 5.6–5.9% |
| 堆图、原生导入对象、Promise 作业和组合状态校验 | 79.2–79.6% | 65.8–66.3% |
| operation 结算与临时数据释放 | 约 0.1% | 0.4–0.5% |

仅堆图与原生目标两段纯校验已占 React 的 83.5–83.7%、四个挂起负载的 73.4–73.8%；带计时点入口与普通入口无稳定偏移（−2.8% 至 +1.6%）。堆插入段含局部校验，不能解释为纯分配成本。

## 可信恢复与诊断审计分离

默认 `Vm::restore` / `Vm::restore_verified` 只接收可信、未经篡改的 VM 镜像，不重查堆图、模块根与 getter、Symbol 表、拒绝通知队列、续体值引用及 Promise 关系；格式、Build ID、绑定指纹和宿主重链接仍检查，重建过程中的局部检查和宿主 codec 仍执行。开发和诊断使用 `Vm::restore_validated`；镜像编码不变。

同一 release 进程内用同一构建和快照交替对照完整校验与可信路径，三批各 15 个配对样本，均验证续跑结果。[原始 CSV](bytecode-loading/trusted-restore/)。

| 负载 | 完整校验（ms） | 可信恢复（ms） | 恢复时间降幅 |
| --- | ---: | ---: | ---: |
| React 首提交后 | 2.209–2.215 | 0.383–0.400 | 82.0–82.9% |
| `await` 成功 | 0.459–0.474 | 0.144–0.151 | 67.9–69.1% |
| `finally` 中 `await` 拒绝 | 0.463–0.475 | 0.143–0.145 | 68.7–69.5% |
| 同步生成器 | 0.470–0.473 | 0.147–0.149 | 68.4–69.0% |
| 异步生成器 | 0.468–0.495 | 0.145–0.156 | 68.7–69.0% |

# `BinaryConst`: 字面量右操作数不再单独发射 `Const`

普通 `Binary` 的右操作数即使是字面量, 也要先执行一条 `Const` 把值写入槽, 再执行 `Binary`。本轮新增 `Instr::BinaryConst { dst, op, lhs, constant }`: 编译器对右侧字面量直接引用常量池, 运行时从构建常量取右值并复用同一套二元运算路径。字面量没有求值副作用, 左侧仍先求值, 因而求值顺序不变.

## 环境与命令

Apple M2 (24 GiB), macOS 27.0.1, rustc/cargo 1.99.1. 基线是提交 `049312b`; 最终版是同一工作树上的改动. 两版各跑 A/B/C 三批, 每批 `--samples 5`, 进程内预热 2 次; 报告使用每批中位数. 复现:

```sh
cargo bench -p yield-harness --bench baseline -- --samples 5
```

基线原始样本复用 [数值快速路径对照](../number-fast/number-a/b/c.csv); 本轮样本在 [binary-const/](binary-const/) 的 `const-a/b/c.csv`, 诊断计数见 `counters.csv`.

## 结果

| 负载 / 指标 | 基线 (µs) | 最终版 (µs) | 变化 |
|---|---:|---:|---:|
| 数值/分支 / `entry_first` | 736.71 | 650.50 | −11.7% |
| 数值/分支 / `entry_steady_5` | 3724.08 | 3250.75 | −12.7% |
| 短调用 / `entry_first` | 207.33 | 184.29 | −11.1% |
| 短调用 / `entry_steady_5` | 1052.62 | 948.54 | −9.9% |
| 静态属性 / `entry_first` | 245.75 | 226.33 | −7.9% |
| 静态属性 / `entry_steady_5` | 1173.71 | 1123.67 | −4.3% |
| 闭包读写 / `entry_first` | 351.25 | 330.96 | −5.8% |
| 闭包读写 / `entry_steady_5` | 1857.21 | 1757.62 | −5.4% |
| `await` 成功 / `entry_steady_5` | 77.62 | 73.62 | −5.2% |
| React 更新 / `entry_first` | 2507.88 | 2445.21 | −2.5% |
| React 首提交 / `entry_first` | 463.62 | 446.58 | −3.7% |

静态指令数: 数值/分支 53 → 47, 短调用 29 → 27, React 首提交 36,363 → 34,735. 产物大小: 1,813 → 1,717 B, 1,424 → 1,392 B, 814,499 → 788,451 B. 异步生成器 `entry_first` +1.7%、稳态 +1.1%, 按样本波动处理.

## 边界

- 只融合右侧的字面量 (`bool`、`null`、`number`、`string`、`bigint`); `undefined`、正则、带替换的模板等仍走原来的 `Const`/专用指令. 左侧仍按源码顺序先求值.
- 字面量没有 ToPrimitive 或 getter 副作用; 含对象、BigInt 混合、Symbol 等的运算仍走通用 `binary_value` 回退.
- 加载校验检查 `lhs`/`dst` 槽和常量池索引. 指令形状变化使 `BYTECODE_VERSION` 从 48 升到 **49**; 快照记录形状未变, `SNAPSHOT_FORMAT_VERSION` 不变.
- `BinaryConst` 与普通 `Binary` 共用 `binary_value`; 上一轮的双 Number 快速路径对两种指令都生效.

# `Instr::Binary` 的双 Number 快速路径

`Instr::Binary` 原先无论操作数类型都构造 `Hint`、检查对象转换并进入 `ops::binary_op`. 当左右操作数都是原始 `Number` 时, 这些步骤不会产生副作用, 也不会触发 ToPrimitive. 本轮在这条路径上直接执行 f64 算术、比较、宽松/严格相等和 ToInt32/ToUint32 位运算; 任一操作数不是 `Number` 时仍走原通用路径.

## 环境与命令

Apple M2 (24 GiB), macOS 27.0.1, rustc/cargo 1.99.1. 基线是提交 `c5942e1`; 最终版是同一工作树上的改动. 两版各跑 A/B/C 三批, 每批 `--samples 5`, 进程内预热 2 次; 报告使用每批中位数. 复现:

```sh
cargo bench -p yield-harness --bench baseline -- --samples 5
```

基线原始样本复用 [构建句柄对照](../build-handle/rc-a/b/c.csv); 本轮样本在 [number-fast/](number-fast/) 的 `number-a/b/c.csv`.

## 结果

| 负载 / 指标 | 基线 (µs) | 最终版 (µs) | 变化 |
|---|---:|---:|---:|
| 数值/分支 / `entry_first` | 791.08 | 736.71 | −6.9% |
| 数值/分支 / `entry_steady_5` | 3958.71 | 3724.08 | −5.9% |
| 短调用 / `entry_first` | 225.46 | 207.33 | −8.0% |
| 短调用 / `entry_steady_5` | 1143.08 | 1052.62 | −7.9% |
| 闭包读写 / `entry_first` | 384.46 | 351.25 | −8.6% |
| 闭包读写 / `entry_steady_5` | 1971.04 | 1857.21 | −5.8% |
| 静态属性 / `entry_steady_5` | 1239.96 | 1173.71 | −5.3% |
| `await` 成功 / `entry_steady_5` | 82.83 | 77.62 | −6.3% |
| React 更新 / `entry_steady_5` | 9945.04 | 9804.12 | −1.4% |
| React 首提交 / `entry_steady_5` | 2018.33 | 2012.88 | −0.3% |

核心数值、短调用、闭包和静态属性的收益跨三批一致; React 更新约 1.4%. 异步生成器 `entry_first` +3.9%、`entry_steady_5` −0.8%, 按小样本波动处理.

## 语义边界

- 两个操作数都是 `Number` 时, `Add`/`Sub`/`Mul`/`Div`/`Rem`/`Exp` 直接使用 f64 运算; 保留 `NaN`、`-0`、无穷和除零语义.
- `Lt`/`Le`/`Gt`/`Ge`、`Eq`/`Ne`、`StrictEq`/`StrictNe` 直接使用 f64 比较; `NaN` 比较为 false, `0` 与 `-0` 按 SameValueZero/严格相等规则相等.
- 位运算复用 `ops::number_to_uint32` 的 ToUint32 规则, `Shl`/`Shr` 按 ToInt32 左操作数处理.
- BigInt、Symbol、字符串、对象、`null`/`undefined` 等任一端或两端不是 `Number` 时回退 `ops::binary_op`, 不改变错误和类型转换顺序.

## 未启用项

只做算术的版本在短调用上约 −4%, 但数值/分支反而约 +2%; 位运算和比较也进入快速路径后, 数值/分支才稳定转为 −6% 到 −7%. 原始样本见 `number-fast/arithmetic-only-a/b.csv`.

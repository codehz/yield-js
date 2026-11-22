# 条件融合与 Number 一元快速路径

在上一轮比较分支融合的基础上，继续消除条件中的布尔物化和通用一元调用：

- `&&` 条件降为连续的假分支链；左侧为假时直接跳到调用方目标，右侧只在前者通过时求值.
- `||` 条件用 `BranchCompareTrue` / `BranchTrue` 在左侧为真时跳过右侧；右侧仍以假分支结束.
- `!x` 使用 `BranchTrue` 判断操作数；`!(a < b)` 直接使用比较真分支，不生成布尔槽.
- `if` / `while` / `do…while` / `for` / 条件表达式继续共享同一套条件降级；`switch` 仍使用严格相等分支.
- `Instr::Unary` 对 `Number` 的 `Neg`、`Pos`、`BitNot`、`ToNumeric`、`Increment`、`Decrement` 直接计算；对象、Symbol、BigInt 和字符串仍走原有的转换与抛错路径.

## 环境与命令

Apple M2 (24 GiB), macOS 27.0.1, rustc/cargo 1.99.1. 基线 `b310d41` 在独立 worktree 中重新构建，最终版在当前工作树；两者各跑 A–E 五批，每批 `--samples 5`，进程内预热 2 次. 表中使用五批中位数. 复现:

```sh
cargo bench -p yield-harness --bench baseline -- --samples 5
```

原始样本在 [condition-fusion/](condition-fusion/)，动态计数在 `counters.csv`.

## 结果

| 负载 / 指标 | 基线 (µs) | 最终版 (µs) | 变化 |
|---|---:|---:|---:|
| React 首提交 / `entry_first` | 450.71 | 447.38 | −0.7% |
| React 首提交 / `entry_steady_5` | 1958.54 | 1936.92 | −1.1% |
| React 更新 / `entry_first` | 2421.71 | 2391.67 | −1.2% |
| React 更新 / `entry_steady_5` | 9453.21 | 9396.54 | −0.6% |
| 静态属性 / `entry_first` | 217.54 | 210.12 | −3.4% |
| 静态属性 / `entry_steady_5` | 1066.54 | 1034.38 | −3.0% |
| 短调用 / `entry_first` | 180.42 | 167.54 | −7.1% |
| 短调用 / `entry_steady_5` | 888.54 | 855.58 | −3.7% |
| 闭包读写 / `entry_first` | 323.96 | 313.58 | −3.2% |
| 闭包读写 / `entry_steady_5` | 1716.88 | 1655.42 | −3.6% |
| 数值/分支 / `entry_first` | 585.08 | 558.96 | −4.5% |
| 数值/分支 / `entry_steady_5` | 2920.75 | 2780.08 | −4.8% |
| `await` 成功 / `entry_first` | 16.75 | 16.17 | −3.5% |
| `await` 成功 / `entry_steady_5` | 70.92 | 67.46 | −4.9% |
| 异步生成器 / `entry_first` | 17.71 | 17.12 | −3.3% |
| 异步生成器 / `entry_steady_5` | 65.71 | 65.38 | −0.5% |

静态指令数: React 首提交 33,645 → 32,676；数值/分支和短调用不变（分别为 43、26）. React 产物 771,011 → 755,935 B，静态帧槽 4,676 → 4,649.

动态分派次数:

| 负载 | 指标 | 基线 | 最终版 | 变化 |
|---|---|---:|---:|---:|
| React 首提交 | `entry_first.dispatches` | 5,149 | 5,005 | −2.8% |
| React 首提交 | `entry_steady_5.dispatches` | 25,735 | 25,015 | −2.8% |
| React 更新 | `entry_first.dispatches` | 38,263 | 37,207 | −2.8% |
| React 更新 | `entry_steady_5.dispatches` | 164,945 | 160,385 | −2.8% |

## 边界

- `||` 左侧若不是可直接分支的比较，仍会先物化为值再用 `BranchTrue` 测试；这不改变求值次数或短路顺序.
- 本轮的 Number 一元快路径只跳过通用 `unary_op` 分派；对象转换、Symbol 拒绝、BigInt 运算和字符串转换仍走原路径. `Math.*` / `Number.is*` 等内建快路径未改动.
- `while (i++)` 仍是 `Increment` 写回后由分支读取结果，没有额外的更新专用指令；后续通用窥孔或专用指令需要单独测量.
- `BranchTrue` 和比较真/假变体都参与分支目标、异常边、槽活性和快照恢复表的 CFG 处理；加载时校验比较运算符、目标、槽和常量索引.
- 指令形状变化使 `BYTECODE_VERSION` 从 50 升到 **51**；快照记录形状未变，`SNAPSHOT_FORMAT_VERSION` 不变.

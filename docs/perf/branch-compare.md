# `BranchCompareFalse`: 比较与条件分支融合

条件中的布尔比较原先要生成一个临时布尔槽: `Const`/`Binary`/`BinaryConst` 写入结果, 再由 `BranchFalse` 读取并判断真值. 本轮把条件位置直接降为 `BranchCompareFalse` / `BranchCompareConstFalse`: 不创建布尔结果槽, 由 VM 求值比较并按结果跳转. 覆盖 `if`、`while`、`do…while`、`for` 条件、条件表达式和 `switch` 的 `===` 测试.

## 环境与命令

Apple M2 (24 GiB), macOS 27.0.1, rustc/cargo 1.99.1. 基线 `a022f87` 在独立 worktree 中重新构建, 与最终版各跑 A/B/C 三批, 每批 `--samples 5`, 进程内预热 2 次; 表中使用三批中位数. 复现:

```sh
cargo bench -p yield-harness --bench baseline -- --samples 5
```

基线原始样本在 [branch-compare/baseline-a/b/c.csv](branch-compare/baseline-a.csv), 最终样本在 `final-a/b/c.csv`, 动态计数在 `counters.csv`.

## 结果

| 负载 / 指标 | 基线 (µs) | 最终版 (µs) | 变化 |
|---|---:|---:|---:|
| 数值/分支 / `entry_first` | 656.21 | 581.67 | −11.4% |
| 数值/分支 / `entry_steady_5` | 3229.75 | 2912.46 | −9.8% |
| 短调用 / `entry_first` | 185.04 | 173.54 | −6.2% |
| 短调用 / `entry_steady_5` | 940.33 | 899.08 | −4.4% |
| 静态属性 / `entry_first` | 230.67 | 220.38 | −4.5% |
| 静态属性 / `entry_steady_5` | 1124.42 | 1058.04 | −5.9% |
| 闭包读写 / `entry_first` | 335.71 | 322.21 | −4.0% |
| 闭包读写 / `entry_steady_5` | 1738.21 | 1692.00 | −2.7% |
| React 更新 / `entry_first` | 2441.54 | 2411.33 | −1.2% |
| React 更新 / `entry_steady_5` | 9578.58 | 9447.79 | −1.4% |
| React 首提交 / `entry_first` | 458.38 | 458.17 | −0.0% |
| React 首提交 / `entry_steady_5` | 1984.54 | 1962.71 | −1.1% |

动态分派次数下降得更明显, 与去掉的 `Const`、比较结果写入和独立 `BranchFalse` 对应:

| 负载 | 指标 | 基线 | 最终版 | 变化 |
|---|---|---:|---:|---:|
| React 首提交 | `entry_first.dispatches` | 5,539 | 5,149 | −7.0% |
| React 首提交 | `entry_steady_5.dispatches` | 27,685 | 25,735 | −7.0% |
| 数值/分支 | `entry_first.dispatches` | 44,042 | 37,384 | −15.1% |
| 数值/分支 | `entry_steady_5.dispatches` | 220,210 | 186,920 | −15.1% |
| 短调用 | `entry_first.dispatches` | 12,293 | 11,268 | −8.3% |
| 短调用 | `entry_steady_5.dispatches` | 61,465 | 56,340 | −8.3% |

静态指令数: React 首提交 34,735 → 33,645, 数值/分支 47 → 43, 短调用 27 → 26. 产物大小: 788,451 → 771,011 B, 1,717 → 1,653 B, 1,392 → 1,376 B. 数值/分支的 `startup_initialize` 从 1.71 µs 到 1.83 µs 属于绝对 0.12 µs 的噪声；异步生成器首入口 +0.7% 也属于该量级.

## 边界

- 融合仅适用于会产生布尔值的比较运算符；算术、位运算、`in`、`instanceof` 等仍走 `Binary` 或各自指令.
- 非字面量操作数走 `BranchCompareFalse`，字面量右操作数走 `BranchCompareConstFalse`；左侧仍先求值. 右操作数不是字面量时不会被提前求值或复制.
- VM 对双 Number 直接返回条件，不创建 `Value` 布尔值；其他类型复用 `binary_value`，因此 BigInt、Symbol、字符串、对象转换和抛错顺序不变.
- 条件比较抛错时，当前异常处理器仍覆盖该 PC；`switch` 的测试使用严格相等，无转换抛错.
- 指令形状变化使 `BYTECODE_VERSION` 从 49 升到 **50**；快照记录形状未变, `SNAPSHOT_FORMAT_VERSION` 不变.

# Math / Number 内建数值快路径

本轮为 `Math` 和 `Number` 的常用入口增加已为 `Number` 的实参快路径，保留所有非 Number 回退:

- `Math.abs`、单参数/双参数 `Math.*` 方法以及 `Math.max` / `min` / `hypot` 对 Number 实参跳过 `ToNumber` 和 roots 包装.
- `Number(value)` 对 Number 直接返回该数值.
- `Number.isFinite` / `isNaN` / `isInteger` / `isSafeInteger` 不构造 coercion roots.
- `Number.parseFloat` / `parseInt` 需要字符串/基数转换时仍按原路径处理.
- `Math.max` / `min` / `hypot` 仍逐个遍历实参, 保留 NaN、Infinity、`-0` 和抛错顺序.

## 环境与结果

基线 `1908500` 在独立 worktree 中构建, 并加入相同的新工作负载 `builtin-math`；最终版为当前工作树. 两版各跑 A/B/C 三批, 每批 `--samples 5`, 取中位数. 工作负载循环 2048 次, 调用 `Math.abs`、`Math.floor`、`Math.max`、`Number.isInteger` 和 `Number.isFinite`, 返回 6,294,556.

| 负载 / 指标 | 基线 (µs) | 最终版 (µs) | 变化 |
|---|---:|---:|---:|
| `builtin-math` / `entry_first` | 2053.92 | 1945.88 | −5.3% |
| `builtin-math` / `entry_steady_5` | 10219.38 | 9774.42 | −4.4% |
| React 首提交 / `entry_first` | 438.17 | 444.46 | +1.4% |
| React 首提交 / `entry_steady_5` | 1943.71 | 1955.33 | +0.6% |
| React 更新 / `entry_first` | 2381.54 | 2413.25 | +1.3% |
| React 更新 / `entry_steady_5` | 9301.38 | 9377.04 | +0.8% |
| 静态属性 / `entry_steady_5` | 1038.92 | 1026.50 | −1.2% |
| 短调用 / `entry_steady_5` | 851.38 | 845.17 | −0.7% |
| `await` 成功 / `entry_steady_5` | 72.42 | 67.83 | −6.3% |
| 异步生成器 / `entry_steady_5` | 65.29 | 65.58 | +0.4% |

原始样本在 [builtin-fast-paths/](builtin-fast-paths/). `builtin-math` 的产物仍为 1,946 B、49 条静态指令；收益来自 VM 内建入口，不来自字节码变化. React 的 +0.6%~+1.4% 与上一轮窥孔的代码布局/波动一致，未观察到稳定回退.

## 边界

- 仅跳过已经为 Number 的实参转换；对象、字符串、BigInt、Symbol 仍走原转换与抛错顺序.
- `Math.max` / `min` / `hypot` 不短路，所有实参仍逐个处理；`Number.is*` 不执行转换.
- 没有改动宿主调用协议、内建函数对象身份或快照.

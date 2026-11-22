# 单用途 Binary/Unary 条件窥孔

直接条件融合仍只覆盖比较、逻辑和 `!`; 形如 `if (a + b)`、`if (obj.x - 1)` 的任意表达式仍会先写一个结果槽再由 `BranchFalse` 读取. 本轮在 CFG 传播后加入窥孔:

- `Binary` / `BinaryConst` / `Unary` 紧邻 `BranchFalse`, 且结果寄存器只有这一个读者时, 替换为 `BranchBinary` / `BranchBinaryConst` / `BranchUnary`, 删除原 `BranchFalse`.
- 只在分支指令不是任何跳转、异常、`await` / `yield` 恢复落点或生成器入口的目标时启用; 否则绕过运算的路径会改变语义.
- 新分支对比较运算复用 `branch_compare` 的 Number 快速路径; 非比较运算仍走 `binary_value`, 一元运算复用 `unary_value`.
- 删除的 PC 经过现有 `compact_dead_instructions` 重映射；槽活性、异常边和快照暂停表由原流程重算.

## 结果

基线 `1908500`, 最终为该窥孔单独改动；各跑 A/B/C 三批, 每批 `--samples 5`, 取中位数. 原始样本在 [generic-branch-peephole/](generic-branch-peephole/), 计数在 `counters.csv`.

| 负载 / 指标 | 基线 (µs) | 窥孔版 (µs) | 变化 |
|---|---:|---:|---:|
| React 首提交 / `entry_first` | 447.38 | 447.33 | −0.0% |
| React 首提交 / `entry_steady_5` | 1936.92 | 1941.42 | +0.2% |
| React 更新 / `entry_first` | 2391.67 | 2402.04 | +0.4% |
| React 更新 / `entry_steady_5` | 9396.54 | 9321.96 | −0.8% |
| 静态属性 / `entry_first` | 217.79 | 213.21 | −2.1% |
| 静态属性 / `entry_steady_5` | 1040.21 | 1029.08 | −1.1% |
| 短调用 / `entry_first` | 173.21 | 166.54 | −3.8% |
| 短调用 / `entry_steady_5` | 862.50 | 852.33 | −1.2% |
| 数值/分支 / `entry_steady_5` | 2781.71 | 2751.25 | −1.1% |
| 闭包读写 / `entry_steady_5` | 1655.96 | 1645.88 | −0.6% |

闭包 `entry_first` 从 313.58 µs 到 329.46 µs, 但该负载的静态指令数仍为 33 (窥孔没有找到可融合模式), 因此是同一二进制的测量噪声；其余差异也处于短时间负载的波动范围内. 静态收益明确：

| 指标 | 基线 | 窥孔版 |
|---|---:|---:|
| React 静态指令 | 32,676 | 32,440 |
| React 产物大小 | 755,935 B | 752,159 B |
| React 静态帧槽 | 4,649 | 4,637 |
| React 首提交 `entry_steady_5.dispatches` | 25,015 | 24,865 |
| React 更新 `entry_steady_5.dispatches` | 160,385 | 158,945 |

## 边界

- 结果寄存器有第二个读者时不融合；`i++` 这类既更新绑定又用作条件的表达式通常因此保留原样.
- 分支是控制流目标、异常落点或恢复入口时不融合.
- 新指令仍覆盖异常边；比较使用 `branch_compare` 的 Number 快路径，非比较使用通用转换.
- `BYTECODE_VERSION` 从 51 升到 **52**；快照记录形状未变，`SNAPSHOT_FORMAT_VERSION` 不变.

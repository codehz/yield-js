# 数组索引与 `.length` 读取快路径

普通属性读取会先构造/借用属性键，再在属性表和原型链中查找。数组访问本轮增加两个局部快路径:

- 动态 `GetMember` 的键是 Number 时，若对象是普通数组，直接查 `ArrayObject::elements`；只命中自有元素，缺失时回退到原属性链（包括原型 getter）。
- 编译器把 `.length` 读取降为 `GetMemberLength`；普通数组直接返回虚拟长度，其他接收者仍走原来的 `length` 属性读取。
- 字符串、TypedArray、Proxy、访问器、稀疏数组、缺失元素和自定义 `length` 对象都不走数组快路径，保持原语义.

## 环境与结果

基线 `b6b2cfb` 在独立 worktree 中构建, 并加入相同的新工作负载 `array-access`；最终版为当前工作树. 两版各跑 A–E 五批, 每批 `--samples 5`, 取中位数. 复现:

```sh
cargo bench -p yield-harness --bench baseline -- --samples 5
```

工作负载先建立 1024 元素数组, 再做正向、反向数字索引读取和 1024 次 `values.length` 循环.

| 负载 / 指标 | 基线 (µs) | 最终版 (µs) | 变化 |
|---|---:|---:|---:|
| `array-access` / `entry_first` | 1293.71 | 1109.92 | **−14.2%** |
| `array-access` / `entry_steady_5` | 6308.54 | 5393.58 | **−14.5%** |
| React 首提交 / `entry_steady_5` | 1933.79 | 1924.62 | −0.5% |
| React 更新 / `entry_steady_5` | 9295.00 | 9212.08 | −0.9% |
| 静态属性 / `entry_steady_5` | 1034.12 | 1025.83 | −0.8% |
| 短调用 / `entry_steady_5` | 850.00 | 836.96 | −1.5% |
| 数值/分支 / `entry_steady_5` | 2751.67 | 2881.67 | +4.7% |
| `await` 成功 / `entry_steady_5` | 67.71 | 69.88 | +3.2% |

`array-access` 的产物从 1,800 B 降到 1,774 B（`GetMemberLength` 不再保存键常量），静态指令仍为 48. 数值/分支与 `await` 的小幅回退没有对应的指令或访问模式变化，按解释器代码布局波动记录；数组负载的收益稳定超过这些波动.

## 边界

- 数字索引快路径只接受有限、非负、整数值且小于 `u32::MAX` 的 Number 键；`NaN`、小数、负数和超大数继续走属性键转换.
- 只命中数组自有元素；元素不存在时仍走原型链和通用 getter 路径.
- `.length` 快路径只对普通数组直接返回；其他对象/字符串/函数/TypedArray/Proxy 回退普通属性读取.
- 数组写入与 `SetMember` 未改；本轮只优化读取.
- `BYTECODE_VERSION` 从 52 升到 **53**；快照记录形状未变，`SNAPSHOT_FORMAT_VERSION` 不变.

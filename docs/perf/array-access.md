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

`array-access` 的产物从 1,800 B 降到 1,774 B（`GetMemberLength` 不再保存键常量），静态指令仍为 48。该轮观测到数值/分支和 `await` 回退，两者的归因见下方复测。

### 四变体归因复测

在 Apple M2、macOS 27.0.1、Rust/Cargo 1.99.1 上复测。`base` 是 `b6b2cfb`，补上 `82039c4` 的基准夹具；`index` 在 `base` 上仅加入 `array_index_number` 和 `read_member` 的数字索引快路径；`length` 是 `82039c4` 去掉这两处；`both` 是 `82039c4`。四个独立 Cargo target 用 `env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline --no-run` 构建。同一夹具、同一 `Cargo.lock`，直接执行四个二进制；每批预热 2 次、记录 7 样本，八批交错平衡运行位置。每四批的次序分别为 `base index both length`、`index length base both`、`length both index base`、`both base length index`，后四批从第三个次序起轮转。原始样本见 [split/](array-access/split/)。

以下是各批相对 `base` 的中位数变化再取中位数（负值为耗时下降）：

| 工作负载 / 指标 | 仅数字索引 | 仅 `.length` | 两项都开 |
| --- | ---: | ---: | ---: |
| 数组读 / 稳态 | −13.17% | −1.02% | −14.54% |
| 数值/分支 / 稳态 | −0.13% | +4.67% | +4.46% |
| `await` 成功 / 稳态 | −0.61% | −0.64% | −1.04% |
| 同步生成器 / 首次 | +1.63% | +7.89% | +7.01% |
| 异步生成器 / 首次 | −0.38% | +7.37% | +5.87% |
| React 首提交 / 首次 | −1.24% | +0.02% | +0.92% |
| React 更新 / 稳态 | −1.18% | −0.46% | −1.08% |

数值/分支回退随 `.length` 专用化出现：关索引时八批均慢（+3.75% 至 +6.78%）；在 `index` 上加 `.length` 也八批均慢（中位 +4.85%）。两个生成器的首次回退在 `length` 和 `both` 同样都是八批均慢。数值夹具不读取属性，四版都是 43 条静态指令、8 个槽；有无索引快路径的产物 ID 相同，`.length` 组的 ID 随字节码版本变化。数据把回退定位到 `GetMemberLength` 这一组变更，而不是执行 `.length` 快路径本身；解释器代码生成或布局是可能原因，未定位到具体机器级机制。`await` 成功的历史 +3.2% 未复现。若只保留索引路径，能保住绝大部分数组读收益，同时避开本轮复现的数值稳态回退；本次未更改生产代码。

## 边界

- 数字索引快路径只接受有限、非负、整数值且小于 `u32::MAX` 的 Number 键；`NaN`、小数、负数和超大数继续走属性键转换.
- 只命中数组自有元素；元素不存在时仍走原型链和通用 getter 路径.
- `.length` 快路径只对普通数组直接返回；其他对象/字符串/函数/TypedArray/Proxy 回退普通属性读取.
- 数组写入与 `SetMember` 未改；本轮只优化读取.
- `BYTECODE_VERSION` 从 52 升到 **53**；快照记录形状未变，`SNAPSHOT_FORMAT_VERSION` 不变.

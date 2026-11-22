# 优化轮次合并记录

各轮优化的改动、结果、边界与未启用项。环境均为 Apple M2（24 GiB）、macOS 27.0.1、Rust/Cargo 1.99.1，release bench profile，基线在独立 worktree 构建、同一 `Cargo.lock` 与夹具，计时复现命令见[性能基线](performance-baseline.md#复现)。只保留对未来有价值的信息：结论、负结果与边界；已过时轮次的原始 CSV 已删除，负结果的原始数据仍在对应子目录。

## 编译器层

### 显式 CFG + 跨块传播与静态属性键（第一批）

对照配置见 [explicit-cfg.md](explicit-cfg.md)；第一批原始 CSV 已删除，结论如下。

- **CFG 常量/复制传播**默认启用：React 产物缩小 4.4%，解码与启动准备两批均改善；数值负载由运行时值决定分支，无稳态加速。离线编译时间增加约 10%，不进入运行时路径。
- **`GetMemberConstKey` 静态属性键**默认启用：静态属性负载少 5,125 次分派，属性探测次数不变；证据支持减少键物化与分派，不支持查表本身变快。`BYTECODE_VERSION` 升为 47。

### 直接环境访问

模块自身绑定直接寻址模块环境；无每轮环境、闭包创建深度唯一时用固定 `(depth, binding)` 读写。实现见 `compiler/src/lower/mod.rs` 的 `direct_bindings` / `function_depths` 与 `vm/src/heap/store.rs`。

- 短调用 −11%、数值/分支 −18%、React 更新稳态约 −5%、闭包 −5%；React 启动的环境链遍历从 2,853 降到 670，稳态后归零。收益来自移除环境链遍历，不是少执行指令。
- 代价：React 产物 +1.5%，解码中位数 +2.6%–5.4%；冷加载评估时不能忽略。
- 边界：共享单元格、模块活绑定、未初始化状态、每轮捕获保持原语义；其余路径保留 `GetBinding` / `SetBinding` 回退。`BYTECODE_VERSION` 升为 48。

### 挂起后重建字面量（未启用）

用 `13 + (await wait(...))` 与先 `await` 再加载 `13` 对照，模拟把已证明不变的 Number 字面量移到恢复侧：少保存一个值只减少 15 B 快照，代价是恢复侧多一次分派。常规挂起负载镜像主要由堆和宿主状态构成；不引入通用重建 pass。数据见 [explicit-cfg/](explicit-cfg/) 的 `remat-*.csv`。

## VM 层

### 属性表与写侧单态缓存

`PropertyTable` 8 键以内用按 key 排序的密集向量，超过回退 `BTreeMap`；`SetMember` 增加每站点单态缓存（接收者身份、键、布局、槽位），命中 32 次后才填入。基线 `b6aa7d0`。

- 静态属性负载 −31.6% 首次 / −27.8% 稳态；React 首提交稳态 +1.6%（预热计数与表分支成本），其余负载在波动内。
- **未启用（负结果）：**
  - 读侧站点缓存：静态属性同样 −32%，但 React 首提交 `entry_first` +9% 左右；读路径站点检查覆盖不了 React 的一次性读取。样本 [property-cache/read-cache-a.csv](property-cache/read-cache-a.csv)。
  - 全局索引表（`BTreeMap<K, usize>` + 密集槽向量）：React 首提交稳态回退约 7%。
- 边界：Proxy、访问器、原型链、异形索引走通用路径；缓存不进快照，恢复后为空。

### 函数索引与调用分派

`FunctionIndices::Dense`（连续 ID 密集 `Vec`，缺失用 `usize::MAX`）与 `Sparse`（按 ID 排序向量二分）；`Dispatch::Script` 在 `classify` 内一次取出 `callee`/`home`/`class`，`invoke` 不再重复访问堆对象。索引是加载后派生元数据，不进 `Build`、`build_id` 或快照。基线 `ee81bd5` → `93f9668`。

- 函数索引：React 启动初始化 −4.4%，`await` 稳态 −7.7%，闭包首次 −4%；短调用 +0.9%、静态属性部分批次 +5.9% 属布局/样本波动。
- classify 复用：短调用首次 −4.1%/稳态 −5.2%，闭包 −3.4%，React 稳态 −2%。
- **未启用（负结果）：**
  - 帧缓冲复用：复用率约 100%，但短调用稳态 +0.6% 到 +2.1%、React +0.7% 到 +1.6%；小帧的池取用与清零成本高于分配。样本 [function-index/frame-pool-*.csv](function-index/)。
  - 小槽内联（`[Option<Value>; 8]`）：`Frame` 变大，数值/分支稳态 +6.8%、React +2.0% 到 +2.4%。样本 [function-index/inline-slots-*.csv](function-index/)。
  - `#[inline]` 提示：React 首提交 `entry_first` 从 −1.9% 变 +4.9%。
- 边界：保持 `this`、闭包环境、异常和求值顺序；VM 不跨线程共享。

### `Rc<Build>` 构建句柄

`Arc<Build>` → `Rc<Build>`，去掉每次脚本激活的原子增减；只读构建与快照内容不变。基线 `b4ebb52`。

- 短调用 −6.1% 首次 / −5.4% 稳态，异步生成器 −4%；React/闭包无稳定收益。
- 未采用裸指针直接借用：略快但需要 unsafe 别名不变量，保留 `Rc` 的可检查生命周期。

## 指令层

以下各轮共享环境与方法（三批各 `--samples 5`、预热 2 次、报告中位数）。

### 双 Number 快速路径（`Instr::Binary`）

左右操作数都是原始 `Number` 时直接 f64 算术/比较/位运算，跳过 `Hint`、ToPrimitive 与 `ops::binary_op`；任一端非 `Number` 回退原路径。基线 `c5942e1`。数值/分支 −6.9%，短调用 −8%，闭包 −8.6%，React 更新 −1.4%。

- **未启用：** 只做算术的版本短调用约 −4% 但数值/分支 +2%；位运算与比较进入快路径后数值/分支才稳定 −6% 到 −7%。样本 [number-fast/arithmetic-only-*.csv](number-fast/)。
- 边界：保留 `NaN`、`-0`、无穷、除零；BigInt、Symbol、字符串、对象等回退 `ops::binary_op`，错误和转换顺序不变。`BYTECODE_VERSION` 升为 49 由下一轮 `BinaryConst` 承接。

### `BinaryConst`

右侧为字面量（bool/null/number/string/bigint）的 `Binary` 直接引用常量池（`Instr::BinaryConst`），不再发射 `Const`；求值顺序不变。基线 `049312b`。数值/分支 −12.7%，短调用 −9.9%，静态属性 −4.3%，React 更新 −2.5%；React 静态指令 36,363 → 34,735。`BYTECODE_VERSION` 升为 49。

### `BranchCompareFalse` 比较分支融合

条件位置直接降为 `BranchCompareFalse` / `BranchCompareConstFalse`，不创建布尔结果槽；覆盖 `if`/`while`/`do…while`/`for`/条件表达式/`switch` 的 `===`。基线 `a022f87`。数值/分支 −9.8% 稳态、分派 −15.1%；React 分派 −7%、稳态 −1.4%。动态分派降幅明显大于耗时降幅。`BYTECODE_VERSION` 升为 50。

### 条件融合与 Number 一元快路径

`&&` 降为假分支链、`||` 用真分支短路、`!x` 用 `BranchTrue`；`Instr::Unary` 对 `Number` 的 `Neg`/`Pos`/`BitNot`/`ToNumeric`/自增自减直接计算。基线 `b310d41`。数值/分支 −4.8%，短调用 −3.7%，React 分派 −2.8%；React 静态指令 33,645 → 32,676。`BYTECODE_VERSION` 升为 51。

### 单用途 Binary/Unary 条件窥孔

`Binary`/`BinaryConst`/`Unary` 紧邻 `BranchFalse` 且结果寄存器只有这一个读者时替换为 `BranchBinary`/`BranchBinaryConst`/`BranchUnary`。仅在分支指令不是跳转、异常、`await`/`yield` 恢复落点或生成器入口目标时启用。基线 `1908500`。稳态收益小（React −0.8%、静态属性 −1.1%），静态收益明确：React 静态指令 32,676 → 32,440、产物 −3.8 KB。`i++` 类双读者表达式不融合。`BYTECODE_VERSION` 升为 52。

### Math / Number 内建数值快路径

`Math.*`、`Number(value)`、`Number.is*` 对已为 `Number` 的实参跳过 ToNumber 与 coercion roots；非 Number 回退原转换与抛错顺序。基线 `1908500`，新增工作负载 `builtin-math`。`builtin-math` −5.3% 首次 / −4.4% 稳态；React +0.6%–1.4% 与上轮布局波动一致。不改字节码、宿主调用协议或快照。

### 数组数字索引读写快路径

- **读取**：`GetMember` 的 Number 键在普通数组上直接查 `ArrayObject::elements`，只命中自有元素，缺失回退原型链。数字索引读收益 −13%（见下）。
- **写入**：`SetMember` 的 Number 键在普通数组上直接调用 `ArrayObject::set_existing_index`，只更新已有可写数据元素，不改 `length`、不查原型链；父提交 `82039c4` 上 `array-write` 工作负载 −45%。
- **`.length` 专用指令已撤除（负结果）**：编译器曾把 `.length` 读取降为 `GetMemberLength`（`BYTECODE_VERSION` 53）。数组读自身 −1%，但**不执行该指令的负载也变慢**：数值/分支稳态 +4.7%、同步/异步生成器首次 +3%–7.9%，四变体归因复测把回退定位到 `GetMemberLength` 这组变更本身（解释器代码生成或布局），撤除后回退消失。结论：**新增 opcode 也可能拖慢不执行它的负载，指令撤除是正常结局**。归因数据 [array-access/split/](array-access/split/)，撤除复测 [array-access/rollback/](array-access/rollback/)。
- 撤除后状态（`BYTECODE_VERSION` 54）：数组读稳态仍 −13%，数组写 −45%，数值/分支回退消失。
- 边界：只接受有限、非负、整数值且小于 `u32::MAX` 的 Number 键；`NaN`、小数、负数、超大数走属性键转换；字符串、TypedArray、Proxy、访问器、稀疏数组、新索引创建、只读/访问器元素全部走通用路径。

## Error 栈位置（字节码 55）

同一台 Apple M2、`rustc 1.99.1`，变更前后各运行两个独立的非诊断 `baseline --samples 9` 进程。下表为各批中位数，稳态按单次执行计，React 更新负载每次执行包含八次提交。

| 指标 | 变更前 A / B | 变更后 A / B |
| --- | ---: | ---: |
| 数值/分支稳态（µs） | 570.09 / 571.82 | 551.58 / 553.42 |
| 短调用稳态（µs） | 175.45 / 174.29 | 173.15 / 174.67 |
| 静态属性稳态（µs） | 207.90 / 209.88 | 208.49 / 207.36 |
| React 首提交（µs） | 442.75 / 443.25 | 430.67 / 439.88 |
| React 更新稳态（µs） | 1803.35 / 1801.49 | 1800.33 / 1791.08 |
| React 编译（ms） | 83.49 / 83.61 | 84.50 / 84.69 |
| React 产物（B） | 702114 / 702114 | 934736 / 934736 |
| React 产物解码（µs） | 702.17 / 710.25 | 792.75 / 786.58 |
| React 启动准备（ms） | 1.726 / 1.719 | 1.672 / 1.680 |
| React 模块初始化（ms） | 1.441 / 1.442 | 1.422 / 1.407 |

没有无 Error 稳态负载在两批中均慢过基线波动。初版按位置扫描每行的 UTF-16 前缀，使 React 编译升至约 217 ms；改用一次构建的稀疏字节修正索引后，不损失坐标精度。

同一 React 输入的体积和 4096 次 `new Error` 循环这两项可用 `env -u YIELD_VM_BENCH_DIAGNOSTICS cargo bench --locked -p yield-harness --bench baseline -- --samples 9 --stack-traces` 重测。循环耗时包含相同的函数调用和对象分配，不包含 VM 启动。

| 位置记录 | React 产物（B） | 循环中位数（ns / Error） |
| --- | ---: | ---: |
| 关闭 | 714152 | 752.44 |
| 开启 | 934736 | 1133.15 |
| 开启且隐藏 vendor | 718892 | 1027.09 |

## 版本演进摘要

`BYTECODE_VERSION`：47（`GetMemberConstKey`）→ 48（direct 绑定）→ 49（`BinaryConst`）→ 50（比较分支融合）→ 51（条件融合/一元）→ 52（窥孔）→ 53（`GetMemberLength`，已撤回）→ 54（数组读写快路径，`.length` 回普通读取）→ 55（Error 栈位置）。这些变更不改变快照记录形状。

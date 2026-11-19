# JS 基础语法特性实现缺口（现状盘点）

> 基线：`docs/design.md`（目标契约）对比当前 `crates/compiler` + `crates/vm` 代码（提交时点）。
> 本文只描述"当前已实现什么、还缺什么"，不是路线图。所有缺口均以编译期 `unsupported syntax: …`（带源码行号）形式拒绝，或以 VM 层缺失值类型呈现。

## 1. 值模型缺口

当前 `Value`：`Undefined / Null / Bool / Number(f64) / String / Symbol(SymbolId) / Object(ObjectId)`。

| 特性 | 目标（design.md） | 现状 | 缺口性质 |
| --- | --- | --- | --- |
| `Symbol` | 支持，含 `Symbol.for/keyFor`、`Symbol.iterator`，快照保留注册表与身份 | 已支持：`Symbol()`/`Symbol.for/keyFor`/`Symbol.iterator`，`PropertyKey::Symbol` 分表存储，快照保留描述、注册表与高水位（iterator 与注册项恒为根）；Symbol 不可跨宿主边界，不能 `new` | 已落地 |
| 正则字面量 | `RegExp` 支持 | 字面量与 `RegExp` 内建均无（落到 fallback 表达式拒绝） | 内建 + 字面量编译 |
| `BigInt` | （设计未列） | 无；`Build::validate` 拒绝非有限数，`1n` 直接走"表达式形式超出子集"拒绝 | 未列入首版契约 |
| 数字语义 | 标准 | f64 直接编码，无 -0/-NaN 规范化问题，但无 `Number` 包装 | 部分覆盖 |
| `type_of` | `"function"`、`"symbol"` 等 | 已含 `"symbol"`；可调用对象报 `"function"`（`exec.rs::unary`），不可调用对象仍是 `"object"` | 部分覆盖 |

## 2. 编译期已拒绝（`lower` 各处显式 `unsupported`）

### 2.1 绑定与函数
- **默认参数**（`lower/mod.rs`，"default parameter"）
- **rest 参数**（"rest parameter"）
- **参数/catch 处解构**（"parameter destructuring"、"catch parameter destructuring"）；变量声明解构（"destructuring declaration"）
- **generator / async generator**（"generator"；async 支持但生成器整体拒绝）
- **匿名顶层/嵌套函数声明**（"anonymous declaration"）

### 2.2 表达式
- **`in` / `instanceof` 运算符**
- **`delete`**：仅当操作数为标识符/成员/可选链时拒绝；`delete` 属性删除无实现
- **实参展开、数组展开、对象展开**（"spread argument" / "array spread" / "object spread"）
- **自增/自减**：目标仅限简单绑定或成员（"update target outside the PoC subset"），其余赋值目标拒绝（"assignment to non-identifier/non-member target"）。算术复合赋值与逻辑赋值（`||=`/`&&=`/`??=`）已支持
- **私有字段**（"private field access"）
- **标签模板**、**带类正则以外的模板特性**：非标签模板已支持；标签模板拒绝
- **lone surrogate 字符串字面量**（"string with a lone surrogate"）
- **非有限数字字面量**（"non-finite numeric literal"，`Build::validate` 双保险）

### 2.3 控制流
- **`for…in` / `for…of`**（同步 for…of 在设计目标内，当前无实现）
- **`with`、`debugger`**
- **`class` 声明**（整个类语法拒绝）
- **`switch` 重复 default**（编译期拒绝，正确但属于实现限制的下游）

### 2.4 顶层
- 顶层只允许**函数声明 + 空语句**；顶层 `await`、`var/let/const`、表达式语句均拒绝
- **import/export**：解析为 `.mjs` 后由子集拒绝（符合契约，但仍列出）

## 3. 运行时语义缺口（编译不拒绝但行为不完整）

| 语义点 | design.md 目标 | 现状 |
| --- | --- | --- |
| `arguments` | 支持（严格模式 `arguments`） | 无任何绑定或降级；引用即"unknown binding"/未知名 |
| 原型链、属性描述符、getter/setter 运行时 | 支持 | 堆只有对象/数组/闭包/promise 等，无 `GetMember` 的原型查找语义说明；getter/setter 仅编译期存在（`DefineProperty` 带种类），行为依赖堆实现 |
| 标准内建 | `Object`/`Array`/`String`/`Number`/`Math`/`RegExp`/`JSON`/`Error` 等 | `BUILTINS` 只有 `["Promise", "queueMicrotask", "Error", "TypeError", "Symbol"]` |
| 完整 ECMAScript TDZ | 明确不做 | `pending_lexicals` 仅本函数静态检查；闭包提前读得 `undefined` |
| 循环每轮新绑定 | 明确不做 | 共享绑定 |
| 名称策略例外 | — | `typeof` 对被局部遮蔽的 import/顶层函数报 `"function"`（有意保留，`names.rs`） |
| Proxy / Reflect / Map / Set / WeakMap | Solid/React 验收前置 | 无 |
| thenable 同化、Promise 组合子 | `all/allSettled/race/any` 等 | Promise 有基础 `then/catch/finally`；组合子未核对 |

## 4. 结构性缺口（影响后续所有语法特性）

1. **值模型单类型化**：加 `BigInt`/`RegExp` 仍要动 `Value`、`type_of`、truthy、堆边表、快照可达性（`heap.rs::write_edges`）与 `SNAPSHOT_FORMAT_VERSION`；Symbol 已按此路径落地（v8）。
2. **顶层不是脚本体**：无法承载顶层 `await`、模块语句、顶层 `var`；若要支持，需要把顶层也建成一个函数帧而非只建函数表。
3. **`for…in`/`for…of` 仍拒绝**：数组已能通过 `values()` 或 `[Symbol.iterator]()` 创建实时值迭代器；快照 v9 保存源数组引用及游标。编译器仍不消费迭代协议，提前退出时的 `IteratorClose` 未实现。
4. **解构与 rest/spread 仍拒绝**：数组解构与展开尚未接入迭代协议；对象解构与展开也未实现。

## 5. 优先级示意（仅现状组织，非承诺）

```mermaid
flowchart LR
  A[Value 扩展: Symbol/function 识别] --> B[迭代协议 for-of/解构/spread]
  B --> C[原型链 + 属性描述符]
  C --> D[标准内建 Object/Array/String/Math/JSON]
  A --> E[Proxy/Reflect]
  F[类语法 + default/rest/解构] --> B
```

## 6. 已知非缺口（明确不做，勿当缺口实现）

- 完整 TDZ、每轮循环独立绑定（见 design.md 与 AGENTS.md）
- generator、async generator、`for await…of`、顶层 `await`、动态模块、`eval`、`Function` 构造、`with`（首版契约外）
- `WeakRef`、`FinalizationRegistry`、共享内存、`Atomics`
- 快照迁移（版本不匹配即拒绝，不做兼容层）

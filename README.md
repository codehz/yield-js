# yield-js PoC

可恢复迷你 JavaScript VM 的第一个垂直切片。当前实现覆盖设计文档中最关键的闭环：

```text
严格模式 JS 子集
  -> Oxc AST
  -> 显式槽位字节码（构建 ID / 恢复表 / 异常表）
  -> async VM（Promise / 微任务 / 续体 / 闭包环境）
  -> 宿主 operation
  -> 快照 / 恢复
```

## 运行

```sh
cargo test --workspace
cargo run -p yield-poc
```

`yield-poc` 编译并执行设计文档中的双 `await` 例子，在两个挂起点分别快照、丢弃 VM、恢复并完成宿主 operation。输出会显示：

- 未捕获的局部值在持久槽中，按 `saved_slots` 保存；闭包共享的绑定仍在调用环境，没有捕获时快照不保留该环境；
- `inspect` / `seed` 在恢复后不会重跑；
- 最终结果 `15`。

## 目录

| 路径 | 内容 |
| --- | --- |
| `crates/vm` | 字节码契约、加载校验、对象表/环境/闭包、Promise、微任务、async 续体、宿主协议、快照恢复 |
| `crates/compiler` | Oxc 0.159.0 解析、严格模式子集、词法绑定、异常降低、CFG 活性分析、槽分配 |
| `crates/poc` | 端到端测试、`tests/cases` 语义用例与可运行示例 |

## 已实现

- 字节码版本、构建 ID 摘要、函数/导入/常量/恢复表/异常表结构校验。
- 顶层脚本在 `Vm::new(build, &mut host)` 验证构建和导入后执行一次；初始化结束后先排空微任务，再交还宿主。`Vm::start` 按名称读取顶层函数声明的当前绑定值并调用；恢复快照不重跑脚本。
- `SlotKind::{Transient, Persistent}`；`ResumeEntry` 保存 `fulfill_pc`、`reject_pc`、结果槽和升序 `saved_slots`。
- 编译期 CFG 活性分析：每个恢复入口先扣除该入口的结果槽，再取并集作为 `saved_slots`；异常边参与活性。
- 不跨挂起点的槽分配为瞬时槽；挂起时丢弃瞬时槽，恢复时只重建指定持久槽。
- 未跨函数共享的绑定在帧槽中；闭包共享的绑定仍在堆环境，快照保留环境、闭包与对象身份。没有共享绑定且不创建闭包的调用不分配环境。
- 函数声明提升；函数表达式、箭头函数、函数值成员调用。`this` 是隐式参数：方法调用传入接收者（给成员表达式加括号不改变接收者），普通调用传入 `undefined`，箭头函数沿词法链读取。
- 脚本函数支持 `call`、`apply`（参数表限数组、`null` 或 `undefined`）与 `bind`。绑定函数的目标、`this`、预置参数及脚本定义的自有属性可跨 `await` 快照恢复；绑定函数不支持 `new`，不自动创建 `prototype` 属性。
- 对象/数组字面量、计算属性键、简写方法（含 `async`）、getter/setter、成员读写、数组 `length` 与索引、复合成员赋值。`new` 创建实例并沿构造函数的 `prototype` 查找属性；自有属性优先，继承访问器中的 `this` 是实际接收者。简写方法不能用作构造函数。
- `in` 检查对象自身及原型链上的属性（包括 Symbol 键），不执行 getter；右操作数不是对象时抛出 `TypeError`。`for…in` 仍不支持。
- `Reflect.ownKeys(obj)` 返回全部自有键：数组索引键按数值升序，其余字符串键及 Symbol 键分别按创建顺序。`Object.keys(value)` 复用该顺序，仅返回可枚举的自有字符串键；字符串原始值按 UTF-16 索引枚举。两者都不遍历原型链或调用 getter。
- `Object.defineProperty(obj, key, descriptor)` 定义或重定义自有数据或访问器属性，遵守不可配置和只读限制。`Object.defineProperties(obj, descriptors)` 按自有键顺序转换描述符映射中可枚举的字符串键和 Symbol 键，全部转换成功后才开始定义；若定义中途失败，已定义的属性不会回滚。`Object.getOwnPropertyDescriptor` 查询属性而不调用其 getter。描述符可跨快照恢复。数组索引支持描述符和稀疏存储；`length` 不可枚举，缩短时不会删除不可配置的元素。原始值包装对象和其余 `Reflect` 操作尚未实现。
- 普通对象和数组都继承 `Object.prototype.hasOwnProperty`（含 Symbol 键）；数组通过共享原型提供同一个 `values` 与 `[Symbol.iterator]` 方法，游标读取实时元素并可跨 `await` 和快照续用。尚无 `Array` 全局名。数组字面量展开和实参展开复用该迭代协议，已求值的元素或实参可跨 `await` 保存。Symbol 键保真身份、不参与 `length`，字符串键与 Symbol 键分别成表存储。
- `try/catch/finally`：异常表、catch 绑定、finally 正常/throw/return 完成状态、嵌套 finally、await 中的待完成状态。
- VM 自有对象表和对象 ID；对象环、Promise 反应、续体与等待关系都按 ID 保存。
- async 函数、`await`、Promise 微任务恢复；已结算 Promise 也必须经微任务恢复。`await` 会同化 thenable。
- 内建包括 `Error`、`TypeError`、`queueMicrotask`、`Promise`（构造、`resolve` / `reject` / `all` / `race` / `any` / `allSettled`，以及共享的 `Promise.prototype.then` / `catch` / `finally`）与 `Symbol`（`Symbol()`、`Symbol.for` / `keyFor` / `iterator`）。内建单例和共享原型进入快照根。`Promise` 反应、thenable 的 `resolve`/`reject` 能力和组合器状态可快照；快照时微任务队列必须为空，未根可达的 thenable 能力会拒绝快照。
- `Error` / `TypeError` 可直接调用或用 `new` 构造，返回可抛出和捕获的对象，提供 `name`、`message` 与简化的 `stack`（只含名称和消息）。VM 生成的 `TypeError` 也使用同一原型和属性。
- 宿主同步导入、可恢复 operation、operation 完成/拒绝、重复完成拒绝。
- 原生对象稳定键 `(type, id)` 包装；恢复前验证绑定，缺失即整体拒绝。
- 快照仅遍历根可达对象；从 pending operation、async 续体、闭包环境、内建单例、顶层脚本环境和宿主提供的脚本根出发。`Symbol.iterator`、`Symbol.toPrimitive` 与注册表项恒为 Symbol 根，普通 Symbol 只从已标记对象的键值中保留。
- 恢复校验：格式/字节码版本、构建 ID、槽值数量、函数/恢复点、异常表、Promise/续体关联、环境是否与函数布局一致、闭包引用、内建根、顶层环境和宿主入口绑定、缺失 operation、缺失 native 绑定、Symbol 引用与注册表。内部原型必须指向存在的非环境对象，且原型链不能成环；普通属性可以互相引用。
- 快照要求微任务队列已排空。

## 当前子集

已支持的主要语法：

- 严格模式脚本顶层支持子集内的声明和语句，不要求存在函数声明；只有直接位于脚本顶层的函数声明可作为宿主入口。支持函数声明提升、函数表达式、箭头函数、`async` 函数与函数内的 `await`。
- `var` / `let` / `const`、块作用域、简单参数。同一函数内可静态识别的 `let`/`const` 提前访问在编译期报错；跨闭包提前读取得到 `undefined`。
- 字面量、非标签模板、对象/数组字面量（数据属性、简写方法、getter/setter、静态/计算键、数组空洞按 `undefined`、数组展开）、成员读写。
- 对象转换优先调用 `[Symbol.toPrimitive]`，并传入 `default`、`number` 或 `string`。默认/数值转换先尝试 `valueOf`，属性键和非标签模板插值则先尝试 `toString`。转换异常可由 `try`/`catch` 捕获。属性键保留 Symbol 身份，模板不能隐式字符串化 Symbol；复合赋值复用转换后的键，可跨 `await` 快照恢复。
- 数组默认字符串化通过 `join` 读取实时元素。函数继承共享的 `toString`，有意返回固定通用文本 `function () { [native code] }`，不保留也不返回源码。数字字符串支持无效输入产生 `NaN`，并识别 `0x` / `0b` / `0o` 前缀。
- 赋值、算术复合赋值、逻辑赋值（`&&=`、`||=`、`??=`）、前后缀 `++` / `--`、`typeof` / `void`、普通函数调用、函数值调用、导入调用、`new`、`this`。调用与构造支持展开数组及实现 `[Symbol.iterator]` 的对象；迭代抛错会中止实参求值并传播异常，不调用迭代器的 `return`。导入调用展开后的值仍受宿主参数类型限制。
- 可选链 `?.` 支持静态成员、计算成员和调用；遇到 `null` 或 `undefined` 时跳过链中剩余的求值，方法调用仍保留接收者。
- `if`、`while`、`do...while`、`for`、`switch`、语句标签、`break`、`continue`、`return`、`throw`、`try/catch/finally`。`break` / `continue`（含带标签的跳转）可穿越 `finally`。
- 同步 `for…of` 支持数组及实现 `[Symbol.iterator]` 的对象，循环头可用简单声明、绑定或成员赋值。退出循环时按迭代协议调用可选的 `return`；继续同一循环或自然耗尽不调用。迭代器与缓存的 `next` 可跨 `await` 快照恢复。

不支持或尚未精确实现的语法在编译期报告 `unsupported syntax: ...` 与源码行列。

## 已知未覆盖

- `super`、对象展开；对象/数组跨宿主边界（当前只在 VM 内使用）。没有 `globalThis`，脚本顶层的 `this` 为 `undefined`。
- 类、标签模板、generator、async generator、顶层 `await`。
- `for…in`、`for await…of`、字符串迭代和 `for…of` 解构目标。
- 每轮循环独立绑定、完整 ECMAScript TDZ、catch 参数解构。
- Proxy、Map/Set/WeakMap、完整内建对象与标准库。
- `Promise.any` 全拒绝时拒绝值为字符串 `AggregateError: All promises were rejected`。Symbol 不能 `new`、不能跨宿主边界，参与强制数值/字符串转换时抛可捕获的 `TypeError` 对象。没有 `Date`、`RegExp`、`JSON` 和 `Math`。
- 运行期 tracing GC（目前有对象表与快照可达性遍历，没有回收器）。
- C ABI、Apple/Android 宿主适配器、React/Solid 验收。

# 已实现内容

当前实现由代码推导的覆盖范围清单。语法子集与未覆盖项见 [`docs/gaps.md`](gaps.md)。

- 字节码版本、构建 ID 摘要、函数/导入/导出/常量/恢复表/异常表结构校验；导出表拒绝重复公开名，目标必须是初始化函数声明的绑定。
- 严格 ESM 支持本地命名导出：`export function`、`export async function`、`export var` / `let` / `const`、`export { local }` 和 `export { local as publicName }`。`Vm::new(build, &mut host)` 验证构建和导入后执行一次模块初始化，再排空微任务。宿主通过 `Vm::get_export` 读取公开名对应的当前绑定值，`Vm::start` 读取并调用该值；未导出的声明（包括名为 `run` 的函数）对宿主不可见。恢复保留活绑定，不重跑初始化。
- `SlotKind::{Transient, Persistent}`；`ResumeEntry` 保存 `fulfill_pc`、`reject_pc`、结果槽和升序 `saved_slots`。
- 编译期 CFG 活性分析：每个恢复入口先扣除该入口的结果槽，再取并集作为 `saved_slots`；异常边参与活性。
- 不跨挂起点的槽分配为瞬时槽；挂起时丢弃瞬时槽，恢复时只重建指定持久槽。
- 未导出且未跨函数共享的绑定在帧槽中；导出和闭包共享的绑定仍在堆环境，快照保留环境、闭包与对象身份。没有共享绑定且不创建闭包的调用不分配环境。
- 函数声明提升；函数表达式、箭头函数、函数值成员调用。`this` 是隐式参数：方法调用传入接收者（给成员表达式加括号不改变接收者），普通调用传入 `undefined`，箭头函数沿词法链读取。
- 脚本函数支持 `call`、`apply`（参数表限数组、`null` 或 `undefined`）与 `bind`。绑定函数的目标、`this`、预置参数及脚本定义的自有属性可跨 `await` 快照恢复；绑定函数不支持 `new`，不自动创建 `prototype` 属性。
- 对象/数组字面量、计算属性键、简写方法（含 `async`）、getter/setter、成员读写、数组 `length` 与索引、复合成员赋值。`new` 创建实例并沿构造函数的 `prototype` 查找属性；自有属性优先，继承访问器中的 `this` 是实际接收者。简写方法不能用作构造函数。
- `in` 检查对象自身及原型链上的属性（包括 Symbol 键），不执行 getter；右操作数不是对象时抛出 `TypeError`。`for…in` 仍不支持。
- `Object.create(proto, descriptors?)` 用对象或 `null` 作为原型，复用 `Object.defineProperties` 的描述符转换。`Object` / `Reflect` 的 `getPrototypeOf` / `setPrototypeOf` 查询或修改内部原型，不调用 getter。成环、改变不可扩展对象的原型，或把 `Object.prototype` 的原型改为非 `null` 时，`Object` 抛出可捕获的 `TypeError`，`Reflect` 返回 `false`；把原型设为其当前值仍成功。`Reflect` 要求对象目标；`Object.getPrototypeOf` 暂不装箱，`Object.setPrototypeOf` 验证新原型后原样返回 `null` / `undefined` 之外的原始值。宿主原生句柄的原型固定为 `null`。
- `Reflect.ownKeys(obj)` 返回全部自有键：数组索引键按数值升序，其余字符串键及 Symbol 键分别按创建顺序。`Object.keys(value)` 复用该顺序，仅返回可枚举的自有字符串键；字符串原始值按 UTF-16 索引枚举。两者都不遍历原型链或调用 getter。
- `Object.defineProperty(obj, key, descriptor)` 定义或重定义自有数据或访问器属性，遵守不可扩展、不可配置和只读限制；`Reflect.defineProperty` 对定义被拒绝返回 `false`，参数转换异常仍会抛出。`Object.defineProperties(obj, descriptors)` 按自有键顺序转换描述符映射中可枚举的字符串键和 Symbol 键，全部转换成功后才开始定义；若定义中途失败，已定义的属性不会回滚。`Object.getOwnPropertyDescriptor` 查询属性而不调用其 getter。描述符可跨快照恢复。数组索引支持描述符和稀疏存储；`length` 不可枚举，缩短时不会删除不可配置的元素。原始值包装对象尚未实现。
- `delete` 支持成员、计算键和可选链；只移除可配置的自有属性，不调用被删除属性的 getter，也不修改原型。不可配置键在严格模式下抛 `TypeError`，`Reflect.deleteProperty` 则返回 `false`。删除数组索引留下空洞，不改变 `length`；删除后重新添加的非索引键排在同类键末尾。删除结果和属性顺序可跨快照恢复。
- `Object.preventExtensions` / `isExtensible`、`seal` / `isSealed`、`freeze` / `isFrozen` 控制及查询对象的扩展性和自有描述符。密封使自有属性不可配置；冻结还使自有数据属性（含数组 `length`）不可写，但保留访问器的 getter/setter，不递归处理属性值。不可扩展对象不能新增字符串键、Symbol 键或数组索引，但仍可调用继承的 setter；可写的数组 `length` 可以增长而不创建索引。`Reflect.preventExtensions` 返回布尔值，与 `Reflect.isExtensible` 一样要求对象参数。宿主原生句柄始终不可扩展。扩展性和描述符限制均跨快照恢复。
- 普通对象和数组都继承 `Object.prototype.hasOwnProperty`（含 Symbol 键）；数组通过共享原型提供同一个 `values` 与 `[Symbol.iterator]` 方法，游标读取实时元素并可跨 `await` 和快照续用。尚无 `Array` 全局名。数组字面量展开和实参展开复用该迭代协议，已求值的元素或实参可跨 `await` 保存。Symbol 键保真身份、不参与 `length`，字符串键与 Symbol 键分别成表存储。
- `try/catch/finally`：异常表、catch 绑定、finally 正常/throw/return 完成状态、嵌套 finally、await 中的待完成状态。
- VM 自有对象表和对象 ID；对象环、Promise 反应、续体与等待关系都按 ID 保存。
- async 函数、`await`、Promise 微任务恢复；已结算 Promise 也必须经微任务恢复。`await` 会同化 thenable。
- 内建包括 `Error`、`TypeError`、`queueMicrotask`、`Promise`（构造、`resolve` / `reject` / `all` / `race` / `any` / `allSettled`，以及共享的 `Promise.prototype.then` / `catch` / `finally`）与 `Symbol`（`Symbol()`、`Symbol.for` / `keyFor` / `iterator`）。内建单例和共享原型进入快照根。`Promise` 反应、thenable 的 `resolve`/`reject` 能力和组合器状态可快照；快照时微任务队列必须为空，未根可达的 thenable 能力会拒绝快照。
- `Error` / `TypeError` 可直接调用或用 `new` 构造，返回可抛出和捕获的对象，提供 `name`、`message` 与简化的 `stack`（只含名称和消息）。VM 生成的 `TypeError` 也使用同一原型和属性。
- 宿主同步导入、可恢复 operation、operation 完成/拒绝、重复完成拒绝。
- 原生对象稳定键 `(type, id)` 包装；恢复前验证绑定，缺失即整体拒绝。
- 快照仅遍历根可达对象；从 pending operation、async 续体、闭包环境、内建单例、顶层脚本环境和宿主提供的脚本根出发。`Symbol.iterator`、`Symbol.toPrimitive` 与注册表项恒为 Symbol 根，普通 Symbol 只从已标记对象的键值中保留。
- 恢复校验：格式/字节码版本、构建 ID、槽值数量、函数/恢复点、异常表、Promise/续体关联、环境是否与函数布局一致、闭包引用、内建根、模块环境和导出绑定、缺失 operation、缺失 native 绑定、Symbol 引用与注册表。内部原型（含修改过的内建共享原型）按身份保存；恢复时拒绝悬空链接、环境对象、原型环和 `Object.prototype` 的非空原型。普通属性可以互相引用。
- 快照要求微任务队列已排空。

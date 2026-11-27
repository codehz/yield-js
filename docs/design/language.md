# 首版语言与运行时语义

> 本文是[总体设计](../design.md)的专题文档：模块执行、语法子集、生成器与异步迭代、显式资源管理、VM 内建和 BigInt。构建输入见 [build.md](build.md)，框架验收见 [framework-acceptance.md](framework-acceptance.md)。

## 模块执行与导出

新建 VM 时先验证构建与宿主导入，再在同步初始化帧执行一次入口模块，并排空由此调度的微任务；未完成的宿主 operation 仍由宿主交付。顶层 `var`、`let`、`const` 和函数声明的绑定属于模块环境；它们可以遮蔽同名全局，但不会创建或覆盖全局变量或 `globalThis` 属性。顶层 `this` 为 `undefined`，给未声明名字赋值不会隐式创建全局。只有显式 `export` 的绑定对宿主公开；未导出的顶层函数和变量不构成宿主接口。快照保留模块环境，恢复时不重播初始化。

## 字节码编译器直接支持的语法

| 类别 | 首版直接支持 |
| --- | --- |
| 模块执行模型 | 输入采用严格 ESM 的解析目标与顶层作用域，编译为同步模块初始化帧。允许从静态宿主模块导入绑定，例如 `import { expect } from 'test'`。支持本地命名导出：`export function`、`export async function`、`export var` / `let` / `const`，以及 `export { local }` / `export { local as publicName }`。未知局部绑定和重复导出名在编译期报错。不支持默认导出、再导出（`export … from` / `export *`）或运行时模块加载。`import.meta` 不是运行时对象：宿主在打包期按定义模块规范 ID 注入完整静态路径上的原始常量，不提供 `resolve` 或 URL 推导。 |
| 表达式 | `number` 与 `bigint` 字面量（BigInt 支持十进制、`0x` / `0o` / `0b` 前缀及数字分隔符，不带小数点或指数）、布尔、字符串、`null`、正则字面量与数组/对象字面量（含空洞、计算属性、展开）；模板字面量与标签模板（标签函数按调用点取得缓存且冻结的模板对象，替换值原样传递）；普通/成员/构造调用与实参展开；算术、位运算、比较、赋值、自增减、短路、三元、逗号、可选链、`??` 与逻辑赋值；`typeof`、`void`、`delete`、`in`、`instanceof`。 |
| 控制流 | `if`、`switch`、`for`、`while`、`do…while`、`for…in`、同步 `for…of`（含提前退出时的 iterator close）、`for await…of`（含异步 iterator close）；标签、`break`、`continue`、`return`、`throw`、`try/catch/finally`。 |
| 异步 | `async` 函数与 `await`；generator（`function*`、`yield`、`yield*`）、async generator（`async function*`）；thenable 同化、异常拒绝、Promise 微任务顺序与已结算 Promise 的异步恢复。 |
| 显式资源管理 | `using` / `await using` 单名声明、C 风格 `for` 声明头、同步与异步 `for…of` 资源头。模块求值仅支持同步 `using`；清理协议与内建见下文。 |

`class`（含 `extends`、字段和私有成员）是一等支持的语法：字节码编译器直接编译类声明与类表达式，不经离线转译降级——转译私有字段会生成额外的弱映射/闭包代码，破坏快照可恢复性和代码量目标。类语义（构造器、继承链、`super`、静态成员、字段初始化、私有字段/方法/访问器）在编译器与 VM 内建层面实现；宿主类声明（`#[host_class]`）与脚本类共用同一套 VM 类对象表示。标签模板同样由编译器直接编译，不经离线构建降级：每个调用点分配一个模板对象，包含冻结的 cooked 字符串数组、冻结的 raw 字符串数组以及只读的 `raw` 属性；cooked 元素在非法转义处为 `undefined`，替换表达式按源码顺序求值并原样作为实参传入，不做字符串转换。模板对象的调用点身份与冻结状态属于堆状态，随快照保存并恢复。直接输入中不支持的语法在编译期报告特性名和源码位置；缺失声明的宿主绑定在装载时拒绝。普通 JS 的可选全局探测保留标准语义（如未声明的 `MessageChannel` 在 `typeof` 中返回 `"undefined"`）。

VM 不支持在运行时加载或编译代码。动态 `import()` 和 `with` 在编译期拒绝；全局 `eval` 是可调用但不可构造的内建函数，直接或间接调用时无论输入为何（包括无参数）都抛出可捕获的 `EvalError`，不转换参数、不解析源码、不读写调用者的词法作用域。全局 `Function` 的调用与构造均抛出 `TypeError`，不编译字符串。

## Generator、async generator 与异步迭代

生成器同样由编译器直接编译，不使用宿主的原生生成器，也不用原生调用栈保存挂起位置。每个生成器函数编译为普通字节码函数加一张独立的生成器恢复表；调用生成器函数只分配生成器对象，不执行函数体。生成器对象是 VM 堆对象，持有 `func_id`、状态（未启动、挂起于 `yield`、执行中、已完成）以及与挂起入口对应的持久槽；其原型提供 `next` / `return` / `throw` 和返回自身的 `[Symbol.iterator]`。

`yield` 降低为显式挂起指令，形状与 `AWAIT` 类似但同步返回：`YIELD input, generator_id` 把操作数交给调用者，并保存该恢复表项选出的持久槽。`next(value)` 同步恢复，把 `value` 写入该入口的结果槽；`throw(error)` 从异常落点恢复，使 `yield` 周围的词法 `try/catch/finally` 按普通异常处理；`return(value)` 进入完成路径，先执行挂起的 `finally` 链，不继续执行 `yield` 之后的普通语句。`yield*` 降低为显式循环：读取委托对象的 `next` / `throw` / `return` 并向其逐次转发，委托是异步迭代器时在每步按 `AWAIT` 挂起；委托异常按同步/异步迭代协议收尾。生成器执行中再次调用 `next` / `throw` / `return` 抛出可捕获的 `TypeError`；完成后 `next` 返回 `{ value: undefined, done: true }`，`return(value)` 返回 `{ value, done: true }`，`throw(error)` 直接抛出 `error`，未捕获的异常使生成器进入已完成状态。

async generator 复用同一套挂起指令；`yield` 的操作数先按 `await` 同化，再交付给请求。`next` / `throw` / `return` 各返回一个 VM Promise，每个请求以 `(种类, 参数, 结果 Promise)` 进入该生成器的请求队列，严格按入队顺序逐个结算，前一个请求结束前不推进生成器；生成器在 `await` 挂起期间到达的请求留在队列中，恢复执行后继续处理。其原型提供 `[Symbol.asyncIterator]`。

`for await…of` 先读取 `[Symbol.asyncIterator]`，不存在时退回 `[Symbol.iterator]`，并把同步迭代器包装成异步迭代。每轮先 `Await` 迭代结果、检查 `done`，再 `Await` 迭代值（同步回退时）；循环体和其中任意 `await` / `yield` 都是普通挂起点。循环提前退出（`break`、`return` 或抛出）时调用迭代器的 `return()` 并等待其结果；收尾本身抛出异常时保持原完成状态。迭代结果不是对象、`next` 不可调用或结果未兑现时抛出可捕获的 `TypeError`。异步迭代协议、`Symbol.asyncIterator` 和同步转异步回退包装都是 VM 内建，不需要离线转译。

## 显式资源管理：`using` 与 `await using`

`using` / `await using` 由编译器直接编译。声明只接受单名绑定，一般声明和 C 风格 `for` 头必须带初始化器，`for…of` / `for await…of` 头必须无初始化器。迭代方式与资源释放 hint 独立，`await using` 不隐式等待初始化器。模块求值为同步帧，即使在嵌套块或循环中也拒绝 `await using`；同步函数同样拒绝，不可达语句不例外。模块顶层同步 `using` 在求值时登记和清理。

依据 ECMA-262 的 [CreateDisposableResource](https://tc39.es/ecma262/#sec-createdisposableresource)、[AddDisposableResource](https://tc39.es/ecma262/#sec-adddisposableresource) 和 [DisposeResources](https://tc39.es/ecma262/#sec-disposeresources)：初始化器求值后、绑定初始化前，对对象按普通属性访问查找并捕获释放方法。同步只查 `Symbol.dispose`；异步先查 `Symbol.asyncDispose`，仅在其为 `null` 或 `undefined` 时回退至 `Symbol.dispose`。非对象资源、缺失或不可调用的方法抛 `TypeError`，访问器异常原样传递，不登记失败的资源。同步空值不登记；异步空值登记无方法记录。退出时按 LIFO 先弹出记录，再以资源为 `this` 无参调用已捕获的方法。同步忽略返回值；异步等待真正异步方法的结果，回退包装忽略同步返回值但将同步抛错转为拒绝。按 `needsAwait` / `hasAwaited` 合并异步空记录的等待边界，不在每条记录后无条件等待。释放失败继续处理其余记录；若已有 throw completion，使用内建 `SuppressedError` 原型构造新异常，`error` 是新错误，`suppressed` 是此前异常（即使为 `undefined`），不设置 `cause`。

降级复用现有 finally 机制：编译器为每个声明作用域生成隐式 `try/finally`，资源进入该作用域的待清理列表，离开路径经 finally 以 LIFO 顺序逐个清理。待清理列表和列表游标是普通活跃逻辑值，按现有活性分析进持久槽；`await using` 的清理调用是普通 `AWAIT` 挂起点，挂起期间快照后，恢复从清理列表的当前位置继续，不重放已完成的清理，也不重新执行声明。生成器在 `using` 块内 `yield` 时，`return()` / `throw()` 的完成路径先执行 pending 的清理，复用 finally 待完成状态；async generator 中 `await using` 的清理挂起期间请求留在队首。`for…of` / `for await…of` 头部的 `using` 按每轮独立作用域清理：每轮结束时清理当轮资源，先于该轮的迭代器关闭；循环提前退出时按同样顺序处理当轮资源。顶层同步 `using` 在模块求值结束前 LIFO 清理；快照只发生在初始化帧之后，清理已全部完成。不引入新 opcode，字节码指令集与恢复表形状不变；well-known Symbol 与 `SuppressedError` 进入快照根集合，按契约升相应版本号。

两个栈遵循 ECMA-262 的 [DisposableStack](https://tc39.es/ecma262/#sec-disposablestack-objects) 和 [AsyncDisposableStack](https://tc39.es/ecma262/#sec-asyncdisposablestack-objects) 原型算法：均提供 `disposed` getter、`use` / `adopt` / `defer` / `move`；同步栈仅有 `dispose` 及其 `Symbol.dispose` 别名，异步栈仅有 `disposeAsync` 及其 `Symbol.asyncDispose` 别名。释放先标记 disposed 并取走记录，重入不重放；`move` 转移记录并将旧栈标记为 disposed。异步入口每次返回新的原生 Promise。登记记录和等待游标是堆对象强边，参与 GC 和快照。共享迭代器原型分别提供对应的释放符号方法，动态调用 `return`。宿主类通过 `WellKnownSymbol::{Dispose, AsyncDispose}` 声明符号成员，保持内部类校验，异步收尾用 `cx.promise(op)`，不新增宿主协议。

## VM 内建与可观察语义

- 支持普通对象和原型链、属性描述符、getter/setter、字符串与 Symbol 属性键、标准枚举顺序、稀疏数组、严格模式赋值失败。实现 `Object` 的创建、原型/描述符查询、`defineProperty`/`defineProperties`、枚举、`assign`、`is`、冻结及冻结检查。不安装旧式 `Object.prototype.__proto__` 访问器；保留 `Object` / `Reflect` 的显式原型操作。`"__proto__"` 仍可用作普通自有属性键，对象字面量的语义不变。
- `call` / `apply` / `bind` 是可调用的 `Function.prototype` 上的自有方法。全局 `Function` 的调用与构造均抛出 `TypeError`，不编译字符串。
- 函数转字符串按设计返回 `function <初始名>() { [native code] }`；共享的 `Function.prototype.toString` 不读取公开 `name`，也不保留或反编译源码。
- 支持 `Array`/`String` 的常用访问、转换、搜索、切片与迭代，以及 `Array.isArray/from`、`push/pop/shift/unshift/splice`、`map/filter/reduce/forEach/some/every/flat`；提供 `Number`、`BigInt`、`Math`、`RegExp`、`JSON` 和标准 `Error` 类型。具体方法和正则用法以锁定构建和回归用例补齐，不以整个内建对象的存在宣称完整兼容。
- 二进制数据由 VM 自带实现，不依赖宿主缓冲区：`ArrayBuffer` 持有唯一的字节块（固定或可调整长度，支持 `resize` / `transfer` / `transferToFixedLength` / `slice`），`DataView` 提供全部数值宽度与 BigInt 的带端序访问，`%TypedArray%` 与其 12 个具体构造器都是 VM 自有对象。视图只记录缓冲区、偏移与可选长度，从不复制字节；整数索引访问与诊断校验共用视图判定。快照保存缓冲区字节与视图元组，诊断入口拒绝越界、未对齐和不一致的记录；`SharedArrayBuffer`、`Atomics` 与分离后重新附加不在此基线。
- `globalThis` 是字节码内建名单中的一个固定根对象，不是对任意宿主全局对象的代理。框架基线可使用原型为 `Object.prototype` 的普通对象；[WinterTC 目标](wintertc.md)将其扩展为 VM 自有的 `EventTarget`。内建名单里的名字（`Math`、`JSON`、`Symbol` 等）以数据属性挂在该对象上；脚本可以通过 `globalThis` 显式访问它们，但裸名称不做属性查找，而是走静态解析。裸名称解析顺序为：局部绑定 → 显式宿主导入 → preload 导出 → 内建 → `undefined` 字面量；解析不到的名字在编译期报错（读取、赋值和除 `typeof` 外的引用都不可见），`typeof 未知名字` 折叠为 `"undefined"`。运行时动态添加到 `globalThis` 的属性不会反向进入编译期的裸名称解析。
- preload 全局：`Vm::new` 执行 preload 初始化器后，把 preload 的显式导出安装为 `globalThis` 上的可枚举、不可配置、仅 getter 的活绑定——每个名字一个捕获 preload 模块环境的 getter 函数，读值时实时读取 preload 绑定槽，不是安装时复制的快照值。安装只发生在 VM 启动路径；快照恢复不重新安装。诊断入口校验 `globalThis` 上这些 getter 的描述符与 preload 记录一致（名字、环境、函数身份）。preload 名字在静态解析中优先于内建；同名局部绑定仍遮蔽 preload 全局。
- 支持 `Symbol`（含 `Symbol.for/keyFor`、`Symbol.iterator`、`Symbol.asyncIterator`、`Symbol.dispose`、`Symbol.asyncDispose`）、同步与异步 iterator 协议、生成器与 async generator 的原型、`Map`、`Set`、`WeakMap`。快照保存 symbol 注册表和引用身份、集合条目与顺序；`WeakMap` 遵循弱键/ephemeron 可达性，只存活键所对应的条目，不能把键提升成强根。
- 框架验收用例需要真正的 VM `Proxy`：首版覆盖普通对象/数组（以及 Date、ArrayBuffer、DataView、TypedArray）目标的 `get`、`set`、`has`、`deleteProperty`、`ownKeys`、`getOwnPropertyDescriptor` 陷阱及相应 `Reflect` 操作；遵守属性描述符与不可扩展目标不变量。快照保存代理、目标、handler 及其捕获环境的身份和关系，不执行 trap 来枚举快照。可撤销代理、代理函数/构造器及其他 traps 不在此基线。
- `for…in` 到达每一原型层时才取得该层的自有键列表；逐键检查时读取实时描述符，不提前读取属性值。快照保存已经记录的键及枚举进度，恢复时不重跑该层的 `ownKeys`。
- `Promise` 由 VM 实现：构造器、`then/catch/finally`、`resolve/reject/all/allSettled/race/any`。快照保留未结算 Promise 的反应闭包、链式结果与 async 续体；不序列化宿主 Promise。`queueMicrotask` 进入同一 VM 微任务队列，不是宿主 timer。`WeakRef` 与 `FinalizationRegistry` 由 VM 实现：目标与 token 是弱边，held 与 callback 是强边；检查点保活集把本轮 `WeakRef` 构造和成功 `deref` 的目标当作强根，在微任务检查点排空后清空。快照保存保活集与待清理任务，捕获裁剪失活弱边且不改源 VM，恢复不执行回调。共享内存、`Atomics` 不在此基线。

### BigInt

`BigInt` 是首版的一等原始值类型，与 `Number` 并列，不依赖宿主 JS 引擎的 BigInt 实现。它不是框架验收基线的一部分，而是首版语言契约的独立补充：需要精确整数运算的脚本不必退回字符串或浮点近似。

- **表示与字面量。** 任意精度有符号整数，没有负零，也没有 `NaN` / `Infinity`。支持带 `n` 后缀的十进制、`0x` / `0o` / `0b` 字面量，允许数字分隔符；字面量不得带小数点或指数，十进制不得使用旧式前导零。构建常量与快照值使用带专用标记的规范十进制字符串编码，不以 JSON 数值承载，避免 binary64 精度丢失。
- **值与比较。** `typeof` 返回 `"bigint"`；`Object.prototype.toString` 识别为 `[object BigInt]`。`===` 在 BigInt 与 Number 之间为 `false`；`==` 按数学值比较（如 `1n == 1`、`0n == 0`、`1n == "1"` 为真）；`<`、`<=`、`>`、`>=` 允许 BigInt 与 Number 混合比较。
- **运算。** `+`、`-`、`*`、`/`、`%`、`**`、`&`、`|`、`^`、`~`、`<<`、`>>` 接受 BigInt 操作数。除法向零截断，`%` 取被除数符号，`**` 的指数为负时抛 `RangeError`。BigInt 与 Number 混合的算术或按位运算抛 `TypeError`，但 `+` 遇到字符串仍按字符串拼接；BigInt 没有无符号右移，`>>>` 抛 `TypeError`。一元 `-` / `~` 保留 BigInt，一元 `+` 抛 `TypeError`。
- **转换与内建。** `BigInt(value)` 接受布尔、字符串、整数 Number、BigInt 以及经 `ToPrimitive` 得到的上述值；非整数 Number 抛 `RangeError`，非法字符串抛 `SyntaxError`。`Number(bigint)` 允许精度丢失；`Math` 方法、`Number.prototype` 方法和一元 `+` 遇到 BigInt 抛 `TypeError`。`String`、模板和属性键使用规范十进制，`0n` 为假值。`BigInt` 不可用 `new` 构造，提供 `BigInt.asIntN` / `asUintN`、`BigInt.prototype.toString(radix?)` / `toLocaleString` / `valueOf` 和 `[Symbol.toStringTag] = "BigInt"`。`Object(bigint)` 创建 BigInt 包装对象。`JSON.stringify` 在序列化 BigInt 时抛 `TypeError`（replacer 可先把它转换成可序列化值），`JSON.parse` 不产生 BigInt，`parseInt` / `parseFloat` 仍只返回 Number。
- **宿主边界。** BigInt 是原始值，但任意精度内容由 VM 持有；`JsValue` 仍是不透明定长句柄。宿主可构造并读取规范十进制文本，但不得把大整数交给 JSON 数值、浮点转换或宿主侧第二套值模型。槽、模块状态、operation 交付值和 Promise 拒绝原因中的 BigInt 与普通脚本值遵循同一套快照编码。

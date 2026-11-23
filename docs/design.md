# 可恢复迷你 JavaScript 虚拟机：总体设计

## 目标

构建一个可暂停、可序列化、可快照与恢复的迷你 JavaScript 引擎，作为原生 UI 的脚本执行层。引擎自行实现可恢复虚拟机，不使用 WebAssembly，也不依赖宿主 JS 引擎的调用栈保存执行状态。

本文描述目标契约，不代表当前 PoC 已实现。VM 不实现平台 UI；宿主提供绑定实现和平台服务，VM 快照保存重建这些绑定所需的全部运行时状态。

## 已确定的设计约束

- 面向应用自有脚本，输入默认按 ECMAScript Module（ESM）解析和执行，隐式启用严格模式；不依赖文件扩展名或 `"use strict"`，也不回退到 Script 模式。不以完整 ECMAScript 或任意 npm 包兼容为目标。
- `BigInt` 是首版的一等原始值类型，由 VM 自有的任意精度整数表示；不依赖宿主 JS 引擎，也不把大整数降级为 `Number`。它与框架验收基线无关，但属于首版语言契约。
- 同步执行阶段和原生函数调用中不要求支持暂停或恢复。
- 宿主静态声明模块函数、类及其原型方法。VM 持有函数值、实例和 JS 值槽；实例身份是 VM 对象身份，不由宿主分配或查表。
- 恢复只有一份状态来源：VM 快照。可达绑定的 JS 引用、非 JS 数据以及 pending operation 的状态全部随快照保存。相同的字节码构建、兼容的静态绑定实现和平台服务是运行前提，不是第二份存档。
- 每种宿主异步操作都必须接受中断，并声明如何从任一 pending 阶段恢复。恢复针对原 VM Promise，不保存原生任务、调用栈、回调指针或 OS 句柄。
- 只支持精确版本恢复：快照格式、字节码版本、脚本构建 ID 和所需绑定契约版本均须匹配；不承诺迁移旧快照。

## 已确认的实现方向

- VM 与 compiler 核心使用 Rust；宿主接口面向 Apple 与 Android，但本项目不实现原生 UI。
- Rust 与宿主之间采用手工设计的 C ABI，以不透明 VM 句柄表达跨调用生命周期状态。
- compiler 复用 Rust JavaScript parser（如 Oxc），转换到项目自有 IR；实现时选择当时最新稳定版本并锁定依赖。Oxc AST 不进入 VM 或字节码契约。
- VM 堆采用 VM 自有对象表/句柄与 tracing GC，支持对象环、闭包共享状态及快照对象身份。
- 首版以能运行锁定版本的 Solid Universal 和 React 自定义 renderer 为兼容基线，按下文的构建产物与行为用例验收，而不是按 ECMAScript 年份或 npm 包名宣称兼容。
- Rust 宏可以生成静态模块描述；编译器、VM 和未来的 TypeScript 声明生成器消费同一份声明，不要求宿主实现统一的字符串分派入口。


## 首版 JavaScript 支持范围

### 兼容目标与构建输入

| 验收目标 | 锁定的运行时入口 | 不包含 |
| --- | --- | --- |
| Solid 非 Web | [`solid-js@1.9.9`](https://unpkg.com/solid-js@1.9.9/package.json) 的生产响应式核心、`solid-js/universal` 的 `createRenderer` 和 `solid-js/store` 的 `createStore`。构建显式选用 `dist/solid.js` 和 `store/dist/store.js`，不能因 `node`/`worker` 条件误选 SSR 入口。 | `solid-js/web`、SSR、浏览器 hydration；不启用依赖浏览器 `MessageChannel` 的可选 `enableScheduling()` 路径。 |
| React 非 Web | `react@19.2.0`、`react/jsx-runtime`、[`react-reconciler@0.33.0`](https://unpkg.com/react-reconciler@0.33.0/package.json) 和 `scheduler@0.27.0` 的生产包；使用自定义 mutation-mode `HostConfig`，按该版本实现调度与事件优先级接口。 | React DOM、React Native/Fabric 平台层、SSR/RSC、hydration 和开发工具。 |

应用可写 JS/TS/JSX/TSX。离线构建负责去除类型、用 Solid 的 universal JSX 转换或 React 的 JSX runtime 转换，并解析 npm 依赖及条件导出、固定生产分支、转换 CommonJS、打成一个自包含的 ESM 入口。`compiler` 接收这份源码、声明的宿主绑定及可选源码映射；不承担 JSX/TS 转译或包解析，VM 不加载其他 JS 模块。转换必须保留 ESM 活绑定和 CommonJS 初始化等可观察行为，并保留入口显式声明的公开导出名及其绑定语义，不把依赖的内部导出自动暴露给宿主。产物只允许保留指向已声明宿主模块的静态导入，不得残留 JS 依赖导入、再导出或运行时 `require`。不得替换框架核心或 scheduler 来通过验收。

新建 VM 时先验证构建与宿主导入，再在同步初始化帧执行一次入口模块，并排空由此调度的微任务；未完成的宿主 operation 仍由宿主交付。顶层 `var`、`let`、`const` 和函数声明的绑定属于模块环境；它们可以遮蔽同名全局，但不会创建或覆盖全局变量或 `globalThis` 属性。顶层 `this` 为 `undefined`，给未声明名字赋值不会隐式创建全局。只有显式 `export` 的绑定对宿主公开；未导出的顶层函数和变量不构成宿主接口。快照保留模块环境，恢复时不重播初始化。

### 字节码编译器直接支持的语法

| 类别 | 首版直接支持 |
| --- | --- |
| 模块执行模型 | 输入采用严格 ESM 的解析目标与顶层作用域，编译为同步模块初始化帧。允许从静态宿主模块导入绑定，例如 `import { expect } from 'test'`。支持本地命名导出：`export function`、`export async function`、`export var` / `let` / `const`，以及 `export { local }` / `export { local as publicName }`。未知局部绑定和重复导出名在编译期报错。不支持默认导出、再导出（`export … from` / `export *`）、运行时模块加载或 `import.meta`。 |
| 表达式 | `number` 与 `bigint` 字面量（BigInt 支持十进制、`0x` / `0o` / `0b` 前缀及数字分隔符，不带小数点或指数）、布尔、字符串、`null`、正则字面量与数组/对象字面量（含空洞、计算属性、展开）；模板字面量与标签模板（标签函数按调用点取得缓存且冻结的模板对象，替换值原样传递）；普通/成员/构造调用与实参展开；算术、位运算、比较、赋值、自增减、短路、三元、逗号、可选链、`??` 与逻辑赋值；`typeof`、`void`、`delete`、`in`、`instanceof`。 |
| 控制流 | `if`、`switch`、`for`、`while`、`do…while`、`for…in`、同步 `for…of`（含提前退出时的 iterator close）、`for await…of`（含异步 iterator close）；标签、`break`、`continue`、`return`、`throw`、`try/catch/finally`。 |
| 异步 | `async` 函数与 `await`；generator（`function*`、`yield`、`yield*`）、async generator（`async function*`）；thenable 同化、异常拒绝、Promise 微任务顺序与已结算 Promise 的异步恢复。 |
| 显式资源管理 | `using` / `await using` 单名声明、C 风格 `for` 声明头、同步与异步 `for…of` 资源头。模块求值仅支持同步 `using`；清理协议与内建见下文。 |

`class`（含 `extends`、字段和私有成员）是一等支持的语法：字节码编译器直接编译类声明与类表达式，不经离线转译降级——转译私有字段会生成额外的弱映射/闭包代码，破坏快照可恢复性和代码量目标。类语义（构造器、继承链、`super`、静态成员、字段初始化、私有字段/方法/访问器）在编译器与 VM 内建层面实现；宿主类声明（`#[host_class]`）与脚本类共用同一套 VM 类对象表示。标签模板同样由编译器直接编译，不经离线构建降级：每个调用点分配一个模板对象，包含冻结的 cooked 字符串数组、冻结的 raw 字符串数组以及只读的 `raw` 属性；cooked 元素在非法转义处为 `undefined`，替换表达式按源码顺序求值并原样作为实参传入，不做字符串转换。模板对象的调用点身份与冻结状态属于堆状态，随快照保存并恢复。直接输入中不支持的语法在编译期报告特性名和源码位置；缺失声明的宿主绑定在装载时拒绝。普通 JS 的可选全局探测保留标准语义（如未声明的 `MessageChannel` 在 `typeof` 中返回 `"undefined"`）。

VM 不支持在运行时加载或编译代码。动态 `import()` 和 `with` 在编译期拒绝；全局 `eval` 是可调用但不可构造的内建函数，直接或间接调用时无论输入为何（包括无参数）都抛出可捕获的 `EvalError`，不转换参数、不解析源码、不读写调用者的词法作用域。全局 `Function` 的调用与构造均抛出 `TypeError`，不编译字符串。

### Generator、async generator 与异步迭代

生成器同样由编译器直接编译，不使用宿主的原生生成器，也不用原生调用栈保存挂起位置。每个生成器函数编译为普通字节码函数加一张独立的生成器恢复表；调用生成器函数只分配生成器对象，不执行函数体。生成器对象是 VM 堆对象，持有 `func_id`、状态（未启动、挂起于 `yield`、执行中、已完成）以及与挂起入口对应的持久槽；其原型提供 `next` / `return` / `throw` 和返回自身的 `[Symbol.iterator]`。

`yield` 降低为显式挂起指令，形状与 `AWAIT` 类似但同步返回：`YIELD input, generator_id` 把操作数交给调用者，并保存该恢复表项选出的持久槽。`next(value)` 同步恢复，把 `value` 写入该入口的结果槽；`throw(error)` 从异常落点恢复，使 `yield` 周围的词法 `try/catch/finally` 按普通异常处理；`return(value)` 进入完成路径，先执行挂起的 `finally` 链，不继续执行 `yield` 之后的普通语句。`yield*` 降低为显式循环：读取委托对象的 `next` / `throw` / `return` 并向其逐次转发，委托是异步迭代器时在每步按 `AWAIT` 挂起；委托异常按同步/异步迭代协议收尾。生成器执行中再次调用 `next` / `throw` / `return` 抛出可捕获的 `TypeError`；完成后 `next` 返回 `{ value: undefined, done: true }`，`return(value)` 返回 `{ value, done: true }`，`throw(error)` 直接抛出 `error`，未捕获的异常使生成器进入已完成状态。

async generator 复用同一套挂起指令；`yield` 的操作数先按 `await` 同化，再交付给请求。`next` / `throw` / `return` 各返回一个 VM Promise，每个请求以 `(种类, 参数, 结果 Promise)` 进入该生成器的请求队列，严格按入队顺序逐个结算，前一个请求结束前不推进生成器；生成器在 `await` 挂起期间到达的请求留在队列中，恢复执行后继续处理。其原型提供 `[Symbol.asyncIterator]`。

`for await…of` 先读取 `[Symbol.asyncIterator]`，不存在时退回 `[Symbol.iterator]`，并把同步迭代器包装成异步迭代。每轮先 `Await` 迭代结果、检查 `done`，再 `Await` 迭代值（同步回退时）；循环体和其中任意 `await` / `yield` 都是普通挂起点。循环提前退出（`break`、`return` 或抛出）时调用迭代器的 `return()` 并等待其结果；收尾本身抛出异常时保持原完成状态。迭代结果不是对象、`next` 不可调用或结果未兑现时抛出可捕获的 `TypeError`。异步迭代协议、`Symbol.asyncIterator` 和同步转异步回退包装都是 VM 内建，不需要离线转译。

### 显式资源管理：`using` 与 `await using`

`using` / `await using` 由编译器直接编译。声明只接受单名绑定，一般声明和 C 风格 `for` 头必须带初始化器，`for…of` / `for await…of` 头必须无初始化器。迭代方式与资源释放 hint 独立，`await using` 不隐式等待初始化器。模块求值为同步帧，即使在嵌套块或循环中也拒绝 `await using`；同步函数同样拒绝，不可达语句不例外。模块顶层同步 `using` 在求值时登记和清理。

依据 ECMA-262 的 [CreateDisposableResource](https://tc39.es/ecma262/#sec-createdisposableresource)、[AddDisposableResource](https://tc39.es/ecma262/#sec-adddisposableresource) 和 [DisposeResources](https://tc39.es/ecma262/#sec-disposeresources)：初始化器求值后、绑定初始化前，对对象按普通属性访问查找并捕获释放方法。同步只查 `Symbol.dispose`；异步先查 `Symbol.asyncDispose`，仅在其为 `null` 或 `undefined` 时回退至 `Symbol.dispose`。非对象资源、缺失或不可调用的方法抛 `TypeError`，访问器异常原样传递，不登记失败的资源。同步空值不登记；异步空值登记无方法记录。退出时按 LIFO 先弹出记录，再以资源为 `this` 无参调用已捕获的方法。同步忽略返回值；异步等待真正异步方法的结果，回退包装忽略同步返回值但将同步抛错转为拒绝。按 `needsAwait` / `hasAwaited` 合并异步空记录的等待边界，不在每条记录后无条件等待。释放失败继续处理其余记录；若已有 throw completion，使用内建 `SuppressedError` 原型构造新异常，`error` 是新错误，`suppressed` 是此前异常（即使为 `undefined`），不设置 `cause`。

降级复用现有 finally 机制：编译器为每个声明作用域生成隐式 `try/finally`，资源进入该作用域的待清理列表，离开路径经 finally 以 LIFO 顺序逐个清理。待清理列表和列表游标是普通活跃逻辑值，按现有活性分析进持久槽；`await using` 的清理调用是普通 `AWAIT` 挂起点，挂起期间快照后，恢复从清理列表的当前位置继续，不重放已完成的清理，也不重新执行声明。生成器在 `using` 块内 `yield` 时，`return()` / `throw()` 的完成路径先执行 pending 的清理，复用 finally 待完成状态；async generator 中 `await using` 的清理挂起期间请求留在队首。`for…of` / `for await…of` 头部的 `using` 按每轮独立作用域清理：每轮结束时清理当轮资源，先于该轮的迭代器关闭；循环提前退出时按同样顺序处理当轮资源。顶层同步 `using` 在模块求值结束前 LIFO 清理；快照只发生在初始化帧之后，清理已全部完成。不引入新 opcode，字节码指令集与恢复表形状不变；well-known Symbol 与 `SuppressedError` 进入快照根集合，按契约升相应版本号。

两个栈遵循 ECMA-262 的 [DisposableStack](https://tc39.es/ecma262/#sec-disposablestack-objects) 和 [AsyncDisposableStack](https://tc39.es/ecma262/#sec-asyncdisposablestack-objects) 原型算法：均提供 `disposed` getter、`use` / `adopt` / `defer` / `move`；同步栈仅有 `dispose` 及其 `Symbol.dispose` 别名，异步栈仅有 `disposeAsync` 及其 `Symbol.asyncDispose` 别名。释放先标记 disposed 并取走记录，重入不重放；`move` 转移记录并将旧栈标记为 disposed。异步入口每次返回新的原生 Promise。登记记录和等待游标是堆对象强边，参与 GC 和快照。共享迭代器原型分别提供对应的释放符号方法，动态调用 `return`。宿主类通过 `WellKnownSymbol::{Dispose, AsyncDispose}` 声明符号成员，保持内部类校验，异步收尾用 `cx.promise(op)`，不新增宿主协议。

### VM 内建与可观察语义

- 支持普通对象和原型链、属性描述符、getter/setter、字符串与 Symbol 属性键、标准枚举顺序、稀疏数组、严格模式赋值失败。实现 `Object` 的创建、原型/描述符查询、`defineProperty`/`defineProperties`、枚举、`assign`、`is`、冻结及冻结检查。不安装旧式 `Object.prototype.__proto__` 访问器；保留 `Object` / `Reflect` 的显式原型操作。`"__proto__"` 仍可用作普通自有属性键，对象字面量的语义不变。
- `call` / `apply` / `bind` 是可调用的 `Function.prototype` 上的自有方法。全局 `Function` 的调用与构造均抛出 `TypeError`，不编译字符串。
- 函数转字符串按设计返回 `function <初始名>() { [native code] }`；共享的 `Function.prototype.toString` 不读取公开 `name`，也不保留或反编译源码。
- 支持 `Array`/`String` 的常用访问、转换、搜索、切片与迭代，以及 `Array.isArray/from`、`push/pop/shift/unshift/splice`、`map/filter/reduce/forEach/some/every/flat`；提供 `Number`、`BigInt`、`Math`、`RegExp`、`JSON` 和标准 `Error` 类型。具体方法和正则用法以锁定构建和回归用例补齐，不以整个内建对象的存在宣称完整兼容。
- 二进制数据由 VM 自带实现，不依赖宿主缓冲区：`ArrayBuffer` 持有唯一的字节块（固定或可调整长度，支持 `resize` / `transfer` / `transferToFixedLength` / `slice`），`DataView` 提供全部数值宽度与 BigInt 的带端序访问，`%TypedArray%` 与其 12 个具体构造器都是 VM 自有对象。视图只记录缓冲区、偏移与可选长度，从不复制字节；整数索引访问与诊断校验共用视图判定。快照保存缓冲区字节与视图元组，诊断入口拒绝越界、未对齐和不一致的记录；`SharedArrayBuffer`、`Atomics` 与分离后重新附加不在此基线。
- `globalThis` 是字节码内建名单中的一个固定根对象，不是对任意宿主全局对象的代理。框架基线可使用原型为 `Object.prototype` 的普通对象；下文 WinterTC 目标将其扩展为 VM 自有的 `EventTarget`。内建名单里的名字（`Math`、`JSON`、`Symbol` 等）以数据属性挂在该对象上；脚本可以通过 `globalThis` 显式访问它们，但裸名称不做属性查找，而是走静态解析。裸名称解析顺序为：局部绑定 → 显式宿主导入 → preload 导出 → 内建 → `undefined` 字面量；解析不到的名字在编译期报错（读取、赋值和除 `typeof` 外的引用都不可见），`typeof 未知名字` 折叠为 `"undefined"`。运行时动态添加到 `globalThis` 的属性不会反向进入编译期的裸名称解析。
- preload 全局：`Vm::new` 执行 preload 初始化器后，把 preload 的显式导出安装为 `globalThis` 上的可枚举、不可配置、仅 getter 的活绑定——每个名字一个捕获 preload 模块环境的 getter 函数，读值时实时读取 preload 绑定槽，不是安装时复制的快照值。安装只发生在 VM 启动路径；快照恢复不重新安装。诊断入口校验 `globalThis` 上这些 getter 的描述符与 preload 记录一致（名字、环境、函数身份）。preload 名字在静态解析中优先于内建；同名局部绑定仍遮蔽 preload 全局。
- 支持 `Symbol`（含 `Symbol.for/keyFor`、`Symbol.iterator`、`Symbol.asyncIterator`、`Symbol.dispose`、`Symbol.asyncDispose`）、同步与异步 iterator 协议、生成器与 async generator 的原型、`Map`、`Set`、`WeakMap`。快照保存 symbol 注册表和引用身份、集合条目与顺序；`WeakMap` 遵循弱键/ephemeron 可达性，只存活键所对应的条目，不能把键提升成强根。
- Solid 响应式 props/store 需要真正的 VM `Proxy`：首版覆盖普通对象/数组（以及 Date、ArrayBuffer、DataView、TypedArray）目标的 `get`、`set`、`has`、`deleteProperty`、`ownKeys`、`getOwnPropertyDescriptor` 陷阱及相应 `Reflect` 操作；遵守属性描述符与不可扩展目标不变量。快照保存代理、目标、handler 及其捕获环境的身份和关系，不执行 trap 来枚举快照。可撤销代理、代理函数/构造器及其他 traps 不在此基线。
- `for…in` 到达每一原型层时才取得该层的自有键列表；逐键检查时读取实时描述符，不提前读取属性值。快照保存已经记录的键及枚举进度，恢复时不重跑该层的 `ownKeys`。
- `Promise` 由 VM 实现：构造器、`then/catch/finally`、`resolve/reject/all/allSettled/race/any`。快照保留未结算 Promise 的反应闭包、链式结果与 async 续体；不序列化宿主 Promise。`queueMicrotask` 进入同一 VM 微任务队列，不是宿主 timer。`WeakRef`、`FinalizationRegistry`、共享内存、`Atomics` 不在此基线。

#### BigInt

`BigInt` 是首版的一等原始值类型，与 `Number` 并列，不依赖宿主 JS 引擎的 BigInt 实现。它不是 Solid/React 验收基线的一部分，而是首版语言契约的独立补充：需要精确整数运算的脚本不必退回字符串或浮点近似。

- **表示与字面量。** 任意精度有符号整数，没有负零，也没有 `NaN` / `Infinity`。支持带 `n` 后缀的十进制、`0x` / `0o` / `0b` 字面量，允许数字分隔符；字面量不得带小数点或指数，十进制不得使用旧式前导零。构建常量与快照值使用带专用标记的规范十进制字符串编码，不以 JSON 数值承载，避免 binary64 精度丢失。
- **值与比较。** `typeof` 返回 `"bigint"`；`Object.prototype.toString` 识别为 `[object BigInt]`。`===` 在 BigInt 与 Number 之间为 `false`；`==` 按数学值比较（如 `1n == 1`、`0n == 0`、`1n == "1"` 为真）；`<`、`<=`、`>`、`>=` 允许 BigInt 与 Number 混合比较。
- **运算。** `+`、`-`、`*`、`/`、`%`、`**`、`&`、`|`、`^`、`~`、`<<`、`>>` 接受 BigInt 操作数。除法向零截断，`%` 取被除数符号，`**` 的指数为负时抛 `RangeError`。BigInt 与 Number 混合的算术或按位运算抛 `TypeError`，但 `+` 遇到字符串仍按字符串拼接；BigInt 没有无符号右移，`>>>` 抛 `TypeError`。一元 `-` / `~` 保留 BigInt，一元 `+` 抛 `TypeError`。
- **转换与内建。** `BigInt(value)` 接受布尔、字符串、整数 Number、BigInt 以及经 `ToPrimitive` 得到的上述值；非整数 Number 抛 `RangeError`，非法字符串抛 `SyntaxError`。`Number(bigint)` 允许精度丢失；`Math` 方法、`Number.prototype` 方法和一元 `+` 遇到 BigInt 抛 `TypeError`。`String`、模板和属性键使用规范十进制，`0n` 为假值。`BigInt` 不可用 `new` 构造，提供 `BigInt.asIntN` / `asUintN`、`BigInt.prototype.toString(radix?)` / `toLocaleString` / `valueOf` 和 `[Symbol.toStringTag] = "BigInt"`。`Object(bigint)` 创建 BigInt 包装对象。`JSON.stringify` 在序列化 BigInt 时抛 `TypeError`（replacer 可先把它转换成可序列化值），`JSON.parse` 不产生 BigInt，`parseInt` / `parseFloat` 仍只返回 Number。
- **宿主边界。** BigInt 是原始值，但任意精度内容由 VM 持有；`JsValue` 仍是不透明定长句柄。宿主可构造并读取规范十进制文本，但不得把大整数交给 JSON 数值、浮点转换或宿主侧第二套值模型。槽、模块状态、operation 交付值和 Promise 拒绝原因中的 BigInt 与普通脚本值遵循同一套快照编码。

### 非 Web 宿主与验收

- 两套 renderer 均在 VM 内执行 JS，调用同一类宿主树能力：创建节点/文本、设置属性/文本、插入/移动/删除、遍历父子兄弟、分发输入事件。节点以绑定实例表示：节点间和事件回调的引用走 VM 托管槽，重建平台节点所需的非 JS 数据随实例写入快照。可用可观察的内存节点树验收，无需先实现 Apple/Android UI，也不提供 DOM、Node 或 React Native 全局环境。
- React 的生产 `scheduler` 选用 [`setTimeout` 回退分支](https://unpkg.com/scheduler@0.27.0/cjs/scheduler.production.js)。框架验收宿主须接入可取消的 timer 唤醒；`performance.now()` 由 VM 提供，恢复延续同一相对时钟。定时器回调由 VM 在独立任务轮次执行并排空微任务，回调抛错走全局错误报告；取消或重复投递不执行回调。树事件也在新任务轮次投递，不从原生调用栈同步重入 VM。此验收要求不等于所有宿主都必须提供定时器。
- **运行验收**：用冻结版本的真实生产包和实际 JSX 构建产物。Solid 验证 signal/memo、响应式 props、嵌套 store、列表节点重排、事件更新和卸载清理；React 验证函数组件、`useState`、effect 及清理、带 key 的子节点重排、Promise 驱动的 Suspense 恢复和卸载。与参考 JS 引擎以 ESM 模式执行同一产物、遵循同一宿主适配器契约时的节点树和事件日志对照。
- **恢复验收**：分别在已挂载、等待可恢复 operation 的边界保存快照。用新的宿主从快照重建节点后投递事件或结算原 operation。断言脚本可见的节点和闭包身份延续、挂载副作用不重跑、后续更新和清理正常；React 另验证挂起的 scheduler timer 恢复后只触发一次、已取消 timer 不执行。不能用重新挂载代替恢复。

## Web API 互操作性设计：WinterTC

以 WinterTC 的 [ECMA-429 第一版（2025 年 12 月）](https://ecma-international.org/publications-and-standards/standards/ecma-429/) §§5–7 为固定基线：**除 WebAssembly 外，列出的所有接口、全局方法和属性均为实现目标**。目标包括上游规范定义的成员及可观察行为，不只是同名全局；解码标签范围按下表有意收窄。纯 VM 功能不依赖宿主接入；有外部依赖的功能仍提供 JS 接口，由可选宿主能力承接外部工作。此目标不改变首版 Solid/React 的验收清单，也不声明 ECMA-429 合规（该标准还要求完整的 ECMA-262 合规）。

| API 组 | 实现目标与归属 |
| --- | --- |
| 事件与取消 | `EventTarget`、`Event`、`CustomEvent`、`DOMException`、`AbortController`、`AbortSignal`。监听器、取消原因与信号关系由 VM 持有；`AbortSignal.timeout()` 复用宿主定时器能力。不引入 DOM 节点或事件树。 |
| 全局错误与拒绝 | `reportError`、`ErrorEvent`、`PromiseRejectionEvent`、`onerror`、`onunhandledrejection`、`onrejectionhandled`。目标全局对象由 VM 实现 `EventTarget`，支持全局监听及上述处理器属性；遵循异常报告和 Promise 拒绝跟踪的通知时机及取消规则。宿主日志是报告出口，不替代脚本通知。 |
| 消息与克隆 | `structuredClone`。补齐规范所需的克隆与转移类型（如 `Blob`、`File`），不要求 Worker。`MessageChannel`、`MessagePort`、`MessageEvent` 暂不作为开发目标：端口配对与任务投递在可恢复 VM 的语义下不易实现（挂起任务与快照恢复边界难以定义），留待后续重新评估。 |
| 定时器与时钟 | `setTimeout`、`clearTimeout`、`setInterval`、`clearInterval` 由 VM 实现脚本语义，由可选宿主能力唤醒。`Performance`、`performance.now()`、`performance.timeOrigin` 完全由 VM 提供。禁止运行时编译，故字符串 timer handler 不支持。 |
| 全局别名与标识 | `globalThis`、`self`、`navigator.userAgent`、`navigator.platform`。`self` 初始值与 `globalThis` 相同；`userAgent` 由宿主提供，默认按 §7 使用符合 HTTP 语法的运行时标识，不假冒浏览器，也不决定 Fetch 出站的 `User-Agent` 头；`platform` 是二进制编译的目标平台 `<target_os>-<target_arch>`。 |
| 编码与 Base64 | `atob`／`btoa` 按 HTML 字符串 API 实现。`TextEncoder` 和 `TextEncoderStream` 仅输出 UTF-8；`TextDecoder` 和 `TextDecoderStream` 仅支持 UTF-8、UTF-16LE、UTF-16BE 及其在 Encoding Standard 中定义的标签，其他标签抛 `RangeError`，不纳入实现目标。支持范围内遵循 Encoding Standard 的增量处理、BOM 和错误语义；两个流接口提供 `readable`／`writable`，增量编解码状态随 VM 快照恢复。 |
| URL | `URL`、`URLSearchParams`、`URLPattern`。覆盖 URL 解析与序列化、`URL.searchParams` 的双向联动及模式匹配。基准 URL 不来自浏览器文档：创建 VM 时由宿主显式提供一个 baseURL（可省略），它是 `new URL(相对URL)` 省略 `base` 实参时的默认基准，也用于 Fetch 相对 URL 解析；省略 baseURL 时无默认基准，相对 URL 按规范抛出 `TypeError`。 |
| 微任务、流与压缩 | `queueMicrotask`；`ReadableStream`、`ReadableStreamDefaultReader`、`ReadableStreamDefaultController`、`ReadableStreamBYOBReader`、`ReadableStreamBYOBRequest`、`ReadableByteStreamController`；`WritableStream`、`WritableStreamDefaultWriter`、`WritableStreamDefaultController`；`TransformStream`、`TransformStreamDefaultController`；`CountQueuingStrategy`、`ByteLengthQueuingStrategy`；`CompressionStream`、`DecompressionStream`。流的队列、锁、背压、待结算 Promise 和增量编解码进度进入 VM 快照。 |
| Fetch 与二进制对象 | `Headers`、`Request`、`Response`、`FormData`、`Blob`、`File`、`fetch`。`FormData` 不引入 HTML 表单元素；VM 实现 Fetch 算法和 JS 对象，外部网络及环境服务通过可选宿主能力接入，见下文。 |
| Console | 全局 `console` 的规范方法、参数处理、分组与计时状态由 VM 管理；输出交给可选宿主 sink，恢复不重放日志。 |
| Web Crypto | 目标接口为 `crypto`、`Crypto`、`CryptoKey`、`SubtleCrypto`；当前仅提供 `crypto.getRandomValues()` 与 `crypto.randomUUID()`。VM 实现参数、类型、用法与可导出性约束，熵源及密码操作由可选宿主能力提供。可导出性与快照可恢复性独立；不可导出密钥不以明文或进程句柄落盘，无法安全恢复的可达密钥应使快照失败。 |
| WebAssembly（范围外） | 缺少整个命名空间，包括 `Global`、`Instance`、`Memory`、`Module`、`Table`、`Tag`、`Exception`、`CompileError`、`LinkError`、`RuntimeError`，以及 `compile`、`compileStreaming`、`instantiate`、`instantiateStreaming`、`JSTag`、`validate`。仅记为标准差异，不纳入当前引擎设计范围。 |

- **全局暴露。** 不引入 `Window`、`WorkerGlobalScope` 或 Worker。非 `[Exposed=*]` 接口在 VM 自有作用域暴露是相对 §6 的设计偏差。新增全局同步纳入 `globalThis` 和静态裸名解析，保留局部绑定遮蔽规则，不把宿主属性动态注入编译器名字表。
- **快照边界。** JS 身份、监听器、流状态和已接收但未运行的普通任务数据由 VM 持有，快照不保存 OS 句柄、原生调用栈或待执行的 VM 微任务。外部操作必须定义中断、交付与恢复；无法捕获一致状态时拒绝快照，不得静默丢弃或重放外部副作用。
- **验收。** 各 API 对照上游规范及适用的 [Web Platform Tests](https://github.com/web-platform-tests/wpt)；另覆盖快照前后身份、任务／微任务顺序、取消、缺失能力和恢复不重复交付。逐项记录实现覆盖与可观察偏差。

### VM 相对时钟

`performance.now()` 返回 VM 内部单调计时器的毫秒数，支持小数；不查询宿主能力，也不依赖系统墙钟。新 VM 从零开始。运行期间（含 VM 等待外部事件）累计本地单调时间；快照只保存在一致切点采样的累计时间，不保存进程单调时钟锚点。恢复以该累计值为基数重新锚定，存档至恢复之间的时间不计入。`performance.timeOrigin` 始终为 `0`；它不是 Unix epoch 或任何绝对坐标，`timeOrigin + now()` 仍只是 VM 内的相对时间，不能用于跨 VM 或跨进程对时。

同一个 VM 计时域也用于 timer 截止时间和 Console 计时。快照恢复不得倒退或重置时钟；宿主唤醒只使用相对延时，不是 JS 时钟的权威。验收使用可控的内部测试时钟，覆盖分数精度、单调性、恢复时间连续性及存档间隔不计入。

### 可选宿主能力

启动 VM 时以独立的类型化服务注册定时器、Fetch、Console 输出和 Crypto 等能力，复用现有 service / operation 生命周期，不要求脚本导入宿主模块、不要求宿主实现全局对象或字符串分派。语言无关的边界传递有类型的记录、字节和代际令牌；宿主不持有脚本函数、`JsValue`、Promise 或流对象，也不从平台回调同步重入 VM。宿主可以在后台线程执行工作，仅在 VM 线程按任务投递；每个脚本任务后进行微任务检查。服务声明、状态 schema 及恢复策略可版本化，活动快照记录声明所需的兼容版本；不保存服务实例。恢复时需重新登记的 operation 缺少匹配能力则在发布前报错；有纯 VM 终结策略的记录不必重连服务。

能力默认关闭，不回退到进程的全局网络客户端、凭据库、文件系统或熵源。全局名字仍存在；需调用外部服务的同步 API 缺能力时抛出可捕获的异常（定时器、Crypto 或 Console 输出默认 `NotSupportedError`），Promise API 按正常时序拒绝（网络 Fetch 为网络错误 `TypeError`，异步 Crypto 为 `NotSupportedError`）。无外部工作时按原 API 行为，例如清除不存在的 timer 是空操作，`new Response()` 和本地 `data:` Fetch 不依赖网络能力。宿主拒绝权限或操作失败由 VM 映射到对应 API 错误；过期令牌、越权投递及协议违约是宿主集成错误，不当作正常结果。

#### Web Crypto 随机 API

`crypto` 全局始终存在。宿主可通过 `Bindings` 在稳定 ID `wintertc.crypto` 注册 `Arc<dyn CryptoService>`；同步 `fill_random(&mut [u8]) -> Result<(), HostFault>` 成功时须填满切片，既不接收脚本值，也不重入 VM。两种随机方法仅在调用时查找服务，不回退到 `Math.random` 或进程全局熵源。无匹配服务（包括类型不符）时抛出 `NotSupportedError`；宿主失败时抛出 `OperationError`，不提交部分写入。快照不保存服务实例、熵源状态或内部临时字节；脚本保留的返回值和缓冲区仍按普通值保存。

#### 定时器

- 宿主只需实现一次性 `arm(token, relative_delay) → registration` 与可取消登记；到期投递 token，不分配 JS 定时器 ID、不执行回调，也不负责周期运算。VM 实现延时转换、HTML 的嵌套最小时延、`setTimeout` / `setInterval` 共用 ID 表及 `clear*` 交叉取消；`AbortSignal.timeout()` 也通过同一唤醒能力驱动。提前唤醒按 VM 时钟重新登记，已取消或重复唤醒忽略。
- VM 保存逻辑 ID、回调和参数引用、种类、周期以及同一相对时钟域的下次截止时间和任务顺序；interval 每次执行后仅在未取消时登记下一次，并按本轮调度，不补跑存档期间的 tick。恢复按保存的截止时间及恢复时钟重新计算相对延时，已到期则入普通任务队列。旧平台登记的迟到投递不会触发新代际回调；切点只接受已纳入 VM 状态的任务或仍可由逻辑记录重建的登记。

### Fetch：完整脚本语义与宿主传输契约

目标是实现 [WHATWG Fetch Standard](https://fetch.spec.whatwg.org/) 中适用于该运行时的 Fetch API 和获取算法，不是只提供 buffered HTTP 便捷函数。按以下边界分工；同一适配器可包装平台 HTTP 客户端，不需要实现第二套 JS Fetch。

#### VM：Web IDL、对象与获取算法

- 实现 `Headers`、`Request`、`Response` 及 Body mixin 的完整公开契约：参数转换与验证、header list 的字节和重复项语义、guard 和不可变 header、`getSetCookie()`、请求默认值和方法规则、`Request` 的信号关联、`Response.error/redirect/json`、类型和 URL 元数据。VM 管理 body 的 null／字节流区分、locked／disturbed／`bodyUsed`、clone/tee、`arrayBuffer`、`blob`、`bytes`、`formData`、`json`、`text`，并实现规范的 MIME／编码、multipart 和 `BodyInit` 提取（包括流式上传与 `duplex`）。`Blob`、`File`、`FormData` 和脚本合成的 Request/Response 不依赖网络。
- VM 驱动 Fetch 算法：解析有效 URL，处理 `about:blank`、`data:` 及规范可解析的 `blob:` 本地资源，缺少资源或不支持的 scheme 走网络错误（不把 `file:` 映射到宿主文件系统）；逐跳处理 `redirect`、URL 列表、重定向上限、方法／请求体变换、不可重放上传和跨 origin header 清理。按规范处理 `mode`、CORS（含预检）、`credentials`、HTTP cache mode、referrer/referrerPolicy、`integrity`、`keepalive` 限制，以及适用的 HTTP 子算法；产生 basic／CORS／opaque／opaqueredirect 过滤结果，不将内部响应泄漏给脚本。HTTP 4xx／5xx 是正常响应；网络、策略与完整性失败按 Fetch 算法映射为网络错误，不伪造 HTTP 状态。
- Fetch 环境由宿主在创建 VM 时显式配置，包含可选基准 URL（即上文 WinterTC URL 的 baseURL，`URL` 构造器与 Fetch 共用同一值）、origin、referrer 和网络权限；影响恢复语义的环境参数随 VM 保存并在恢复时核对，不从启动机器猜测。无基准 URL 时不接受相对字符串 URL；未配置 origin 则使用不透明 origin，按相应规范处理同源判断、CORS 和 `Origin` 序列化。按条件使用默认 User-Agent。无 `Window`／导航、Service Worker 或嵌入文档：不模拟这类环境的拦截、文档策略或页面卸载；记录浏览器上下文不适用的路径，不静默忽略本运行时适用的安全检查。`keepalive` 不承诺跨 VM 销毁或快照恢复继续传输。

#### Host：单跳 HTTP、背压与环境服务

- 最小 HTTP 适配器接收 `start(token, HopRequest) → registration`，其中含绝对 URL、方法、保序的 header 字节对、无体／已知长度／未知长度上传描述及本跳的凭据约束；登记提供 `send_upload(bytes)`、`finish_upload()`、`grant_download(bytes_limit)`、`cancel()`。名称仅示意接口方向，不锁定 ABI。单跳指 HTTP 交换，不指整次 Fetch；重定向、预检与后续请求由 VM 另发，平台不得自动重定向、应用浏览器响应过滤、合并 `Set-Cookie` 或擅自重试已可能发送的请求。适配器负责 DNS、连接、代理、TLS、HTTP framing 等平台工作；单跳传输输出去 framing、未自动解压的字节，由 VM 按适用规则解码，避免重复解压或丢失 header 信息。
- 投递类型限定为上传额度、最终 `ResponseHead { status, status_text, headers }`、响应字节、EOF 或类型化失败；需要观测的中间响应保留为单独事件。上传按宿主额度从原 VM 流读取并送入适配器，下载按 VM 授予的字节额度投递，不能以无界缓冲替代流。额度和字节所有权、单跳事件顺序、唯一终结及取消明确写入协议；最终 headers 可先于上传 EOF，最终响应体按规范在可暴露时交付，不要求提前读完。`AbortSignal` 或 body 取消由 VM 按 Fetch/Streams 算法转成取消，事务终结后忽略迟到结果。普通网络失败在响应交付前拒绝原 Promise，交付后只使相应未结束的体流出错；中止遵循规范的 reason 与阶段，不以宿主错误字符串替代脚本原因。
- 每个实际出站的 URL（含重定向、预检和连接重试）都受宿主授权及连接层策略约束；不能把 CORS 当作宿主网络权限。平台客户端应禁止隐式共享 cookie、认证和缓存。可按需注入隔离的 cookie／HTTP 认证／缓存服务，并遵循 VM 为每一跳传入的 origin、分区及 credential/cache mode；无可选存储时是空凭据库、缓存 miss 或按模式返回网络错误，不降级 `only-if-cached` 为联网。宿主可配置请求体、响应体和并发资源限制，超限走可诊断的错误，不能截断成成功。平台服务是外部环境，不是保存 VM Fetch 进度的第二份存档。

#### Fetch 快照与验收

在一致切点保存原 VM Promise、Fetch 状态、已接入 VM 的响应和流关系、排队的交付及取消状态；不保存 socket、TLS 会话、平台请求 ID、宿主缓冲或原生回调。捕获快照不取消源 VM 的登记；源 VM 停止时由其宿主释放登记，恢复实例不依赖源 VM。已完整进入 VM 的响应在恢复后继续由 VM 消费；涉及活动外部传输的恢复基线是**中断，不重发**：响应交付前以 `AbortError` 拒绝原 Promise，交付后不改变已兑现结果，使仍依赖传输的流按中断出错，并清理上传源和取消监听。此终结只在恢复发布后按任务顺序发生；若切点有已归入 VM 的终结事件，先按记录顺序消费，不重复结算。旧代际的迟到结果无效；不能取得一致切点则拒绝捕获。请求方法、已发送字节或 host operation ID 均不作为重放安全证明。

先以可控传输验收一次单跳接入，再运行适用的 WPT Fetch 测试及本地 HTTP 测试：覆盖 header guard 与重复值、body 消费／clone、慢速双向流与背压、预检和各重定向模式、凭据和响应过滤、缓存／完整性错误、缺能力及中止竞态。分别在上传中、等待响应头、响应头已交付、读体中和 EOF 后做快照往返；断言原 Promise 或流按阶段终结、已完成的体可读、宿主没有重复请求。文档与测试要区分脚本侧规范语义、宿主未提供能力以及有意的运行时偏差。

## 总体架构

```mermaid
flowchart LR
    A[自包含 ESM 源码] --> B[解析与编译]
    B --> C[版本化字节码]
    C --> D[VM：栈、帧、堆、Promise]
    E[静态宿主模块描述] --> B
    E --> D
    D <--> H[宿主函数、codec 与 operation 策略]
    D <--> F[单一快照：对象图、绑定数据与 pending operations]
```

### 编译器

`compiler` 是唯一接收 JS 源码的运行核心模块。它复用 Rust JavaScript parser，以 ESM 为解析目标，将离线构建输出转换为项目自有 IR；负责校验语法子集、解析词法绑定、降低异常与异步控制流、分析绑定逃逸与活性、省略可证明不可观察的绑定环境并分配帧槽，最后输出版本化字节码。不支持的语法在编译期报错。源码映射与变量名等调试信息作为可选旁路产物，不参与执行或恢复。

### 虚拟机

`vm` 只接受字节码构建产物及宿主绑定，不接收源码、AST 或编译器 IR，不依赖 `compiler`。它拥有字节码契约及加载验证，显式管理脚本栈、对象堆、彼此独立的模块环境与全局环境、Promise 和 async 续体。VM 堆使用自有对象表/句柄与 tracing GC。编译器遵循该契约；发布产物可只部署 `vm` 与字节码，不携带解析器。恢复时必须先加载同一构建的字节码，不重新编译源码。

### 宿主接口

详细展开（非最终草案）见 [`host-bindings.md`](host-bindings.md)。

- 提供稳定的 C ABI 宿主接口，并管理 VM 句柄的创建、恢复、执行与释放。
- 模块初始化完成后，宿主可按公开导出名读取当前值或调用当前可调用值；公开名可不同于局部名。导出是活绑定，不缓存初始化时的值，也不给宿主提供直接改写绑定的接口。未知导出名和调用不可调用值均报错，不回退到查找顶层声明或全局对象。

#### 静态模块与原型

每份模块描述声明模块名、导出函数及其签名、类的槽布局与方法、可选的构造器导出，以及所用 codec 和 operation 类型。编译器从描述中取得导入清单，VM 装载时把声明标识链接到注册的实现。函数和方法都是一等 JS 函数值；读属性走普通原型链，调用时传入实际 `this`。宿主不用实现 `call_native`、按名字读取原生属性，或为每个实例制造方法函数。

下面以 `test` 模块的 `expect` 导出为例：`Matcher` 在模块内静态注册，只有工厂函数向 JS 返回其实例。Rust 宏语法仅示意，不规定最终命名或要求使用 builder：

```rust
#[host_module("test")]
mod test {
    #[host_class]
    struct Matcher {
        #[js_slot]
        actual: JsValue,
    }

    #[export]
    fn expect(cx: &mut CallCx, actual: JsValue) -> JsResult<Instance<Matcher>> {
        Matcher::alloc(cx, MatcherSlots { actual })
    }

    #[methods(Matcher)]
    impl Matcher {
        #[method(name = "toEqual")]
        fn to_equal(cx: &mut CallCx, this: This<Matcher>, expected: JsValue) -> JsResult<()> {
            let actual = this.actual(cx);
            check_own_data_equal(cx, actual, expected)
        }
    }
}
```

一个 VM 中的 `Matcher` 实例共享原型和方法：`a.toEqual === b.toEqual`。方法检查接收者是否具有 `Matcher` 的内部槽；裸调或用仅伪造了原型的对象作 `this`，抛出可捕获的 `TypeError`。跨 `await` 保存方法时用 `a.toEqual.bind(a)`，或保留 `a` 并作成员调用。注册类型不隐式导出 JS 构造器：上述公开签名足以生成 `export function expect(actual: unknown): Matcher` 和带有 `toEqual(expected: unknown): void` 的 `interface Matcher`，不生成可 `new` 的值，也不公开内部槽。

#### JS 值槽与非 JS 数据

- `JsValue` 句柄仅用于当前 VM 的本次同步调用，不可藏在 Rust 数据或注册闭包中供下次调用。要长期持有任意 JS 值或另一个绑定实例，写入声明的 VM 托管槽。槽不是可枚举或可伪造的 JS 属性，而是对象图的强边；与普通引用一起追踪，保留身份、别名和环，不把所有实例提升为根。
- `expect(x)` 把 `x` 本身放进槽，不投影或复制对象图。`toBe` 比较原值（`SameValue`）；`toEqual` 在调用方法时读取当时的自有可枚举数据，不执行 getter 或 Proxy trap。因此在 `expect(obj)` 之后修改 `obj.x`，后续比较能看到修改。断言失败以脚本可捕获的异常返回。
- 需要 Rust 状态的实例可附带有类型、有版本的 payload。绑定提供 `encode(&T, &EncodeCx, &mut PayloadWriter)` 和 `decode(&mut DecodeCx, &mut PayloadReader) -> Result<T, …>`；两者均可报告错误。普通 `serde` 类型可用适配器，不要求所有类型只实现标准序列化 trait。**上下文提供能力，快照提供状态**：例如用 `DecodeCx` 的平台工厂和快照里的属性创建节点，而不是去旧宿主按节点 ID 取回对象。payload 不得暗藏指向 VM 对象的 ID；这类边一律走槽，无需给绑定作者另设追踪协议。
- 会影响后续调用的模块级可变状态属于 VM 托管的模块记录，作为持久根保存；JS 引用和非 JS 数据分别遵循上述槽和 codec 规则。静态模块描述只包含声明及实现入口，不持有某个 VM 的实例状态。

## 字节码与续体布局

### 构建产物

字节码采用显式槽位操作数，不保留跨挂起点的隐式操作数栈。构建产物包含 `bytecode_version`、常量池、宿主导入表、函数表及指令、模块初始化帧的函数 ID，以及从公开导出名到模块绑定的导出表。导出绑定保留在模块环境中，不降为初始化帧的临时槽；加载时校验导出名唯一且目标是有效模块绑定。常量池只放不可变字面量与静态描述；每次求值应创建新身份的对象不得提升成共享常量。BigInt 常量按规范十进制字符串加专用标记保存并进入构建摘要，不以 JSON 数值承载。`build_id` 是执行相关字节码内容的摘要，覆盖恢复表、生成器恢复表、异常表、宿主导入声明及导出表，不含源码映射等旁路数据。摘要使用稳定的非加密哈希，只用于检测内容错配，不提供对抗篡改的安全性。`build_id` 的摘要输入使用固定的二进制编码，不构造 JSON 中间文本；编码选项与实现版本属于构建身份契约。构建产物由 `Build::to_bytes` / `Build::from_bytes` 编码为带 `YIELDBLD` magic 和 `BUILD_FORMAT_VERSION` 的 bincode 固定整数小端容器；加载时先解码再校验，不以 JSON 作为产物容器。

每个函数有构建内的 `func_id`、入口、瞬时槽数、持久槽数、异常表、恢复表，以及仅生成器函数拥有的生成器恢复表。`AWAIT input, resume_id` 引用函数内的恢复表项，不以运行时栈指针或下一条指令的 PC 标识续体。表项为：

```text
ResumeEntry {
  fulfill_pc, fulfill_dst,  // 写入收到的值，再进入成功落点
  reject_pc,  reject_dst,   // 写入拒绝原因，再进入异常落点
  saved_slots              // 严格升序、无重复的持久槽编号
}
```

恢复落点是专用入口。拒绝落点按原 `await` 的词法位置进入 `catch` / `finally`，无处理器时拒绝外层 async Promise；不从恢复入口的物理 PC 推断处理器。结果槽由恢复路径写入：不因作为结果槽而保存旧值；若该旧值在另一条路径仍然活跃，编译器不得在那条路径上覆盖它。

生成器函数用 `YIELD input, generator_id` 引用独立的生成器恢复表项；`next` / `throw` 各写入自己的结果槽，`return` 不写结果槽而进入完成路径：

```text
GeneratorEntry {
  next_pc,   next_dst,    // next(value)：写入收到的值，再进入正常落点
  throw_pc,  throw_dst,   // throw(error)：写入抛出值，再进入异常落点
  return_pc,              // return(value)：进入 finally / 完成路径
  saved_slots             // 严格升序、无重复的持久槽编号
}
```

`return(value)` 的值在 `finally` 执行前记入显式的待完成状态，不依赖结果槽；`finally` 内可以再次 `await` 或 `yield`，完成状态与其他挂起点一样参与活性分析和快照。

### 固定槽位与快照保留规则

| 状态 | 存放位置 | 挂起时怎么处理 |
| --- | --- | --- |
| 不跨任何挂起点的值，如局部临时值、已消费的实参 | 瞬时槽 `T[n]` | 整组丢弃；不写快照，不作为挂起帧的堆根 |
| 某个挂起点之后可能读到的值 | 函数内固定的持久槽 `P[n]` | 只保留当前 `resume_id` 对应 `saved_slots` 中的槽 |
| 闭包共享的可变绑定 | 堆上的环境或单元 | 从活跃引用遍历，保存同一对象身份；不可把单元内容复制成帧私有值 |
| 未被闭包捕获的绑定，含循环头 `let` / `const` | 帧槽；跨挂起时按活性决定 `P`，否则 `T` | 循环各轮复用同一槽；未被捕获的循环头绑定不复制到每轮环境 |
| 指令、只读常量、恢复表、静态异常处理器 | 字节码构建产物 | 不写快照，用构建内 ID 或静态引用定位 |
| async 返回 Promise、等待关系 | VM 的 Promise / 续体记录 | 保存引用与关系，不把它们混入普通局部槽图 |
| 生成器实例与挂起位置 | VM 堆上的生成器对象（`func_id`、状态、恢复入口、持久槽） | 只保存该入口 `saved_slots` 中的槽；瞬时槽丢弃，生成器对象本身按普通堆对象遍历 |
| async generator 的请求队列 | VM 堆上的请求记录与结果 Promise | 按入队顺序保存请求种类、参数、结果 Promise 及队列游标 |

`P[n]` 在一个函数内布局固定，编号只需在同一构建内稳定。首版不复用不同跨挂起逻辑值的持久槽；无需为快照压缩重排运行帧。实际挂起时清除未列入 `saved_slots` 的持久槽，并释放瞬时槽存储；保存和恢复都按表中的槽顺序进行，不扫描整帧来猜测哪些值还活着。`undefined` 是有效值，不能用它代替“槽未初始化”。词法绑定在作用域入口写成普通 `undefined`，不另存 TDZ 标记。

`const` 的不可变性针对绑定而非绑定值。声明初始化（含循环声明的每轮写入）不受此限制；其他写入在完成该赋值所需的前置求值后抛出可捕获的 `TypeError`。编译期按词法绑定确定写入规则，快照不需要保存绑定可变性。

### 编译期分析与加载验证

1. 在降低后的控制流图上做逆向活性分析。图包含正常落点、拒绝落点、生成器恢复落点和异常处理边，包括 `catch`、`finally`、循环回边与隐式读写。挂起点需要保留的是各恢复入口的 live-in 并集：`AWAIT` 有兑现/拒绝两个入口，`YIELD` 有 next/throw/return 三个入口；每个入口先扣除由 VM 写入的结果槽（`return` 入口没有结果槽），可证明无后续读取才省略。结果不依赖运行时猜测 Promise 最终会成功还是失败。
2. 将跨挂起活跃的逻辑值分配到固定 `P` 槽，其余用 `T` 槽；在每个 `AWAIT` / `YIELD` 处生成该点的 `saved_slots`。已捕获绑定的跨挂起引用指向环境或共享单元；`this`、实参、环境引用等按普通逻辑值分析，不能假定恢复时还能从原调用帧拿到。
3. 将 `finally` 中待完成的 `return`、`throw`、`break` 或 `continue` 降低为显式控制状态（种类、值及适用时的静态目标标签），参与同一活性分析。`finally` 内再 `await` 或 `yield` 时保留仍活跃的待完成状态；若 `finally` 自身以新的 abrupt completion 退出，按 JS 语义覆盖旧状态。无需快照化原生异常栈。
4. VM 加载时检查版本、操作数与槽编号、指令边界、控制流/异常目标、恢复表及生成器恢复表；只有对应的 VM 恢复边能进入专用恢复入口。数据流校验要求 `AWAIT` / `YIELD` 保存的每个槽在挂起前都有可编码状态；以 `saved_slots` 加上该入口写入的结果槽作为恢复入口的已定义集合，沿正常及异常边校验读前有定义，汇合处取交集。每个入口路径都要检查，不接受遗漏的活跃槽；不要求 VM 重做源码分析或拒绝多保留的槽。

### 编译期优化：绑定与环境逃逸

目标是在不改变可观察语义、快照格式和恢复规则的前提下，省略可证明不可观察的绑定环境。分析是纯静态的：证明失败一律退回保守代码生成，不存在按运行期值或 GC 时序改变优化决策的路径。

#### 逃逸判定

本子集中脚本无法取得绑定单元格的引用：动态加载和 `with` 不受支持，`eval` 和 `Function` 不执行输入，宿主也不能访问脚本绑定。因此绑定单元只可能经嵌套函数闭包逃逸。对一个绑定，下列任一条件成立即视为逃逸：

- 任一嵌套函数读或写该绑定，包括多层嵌套、只读捕获和只写捕获。
- 绑定属于模块环境或导出表。循环头 `let` / `const` 不会命中这一条，它们由下面的每轮环境规则单独判定。
- 将来的调试器作用域查看、源码级断点或宿主反射可以观察它。首版没有这些能力；引入对应能力时必须把该构建视为逃逸，或禁止优化。

未逃逸意味着所有读写都发生在拥有该绑定的函数内，且没有闭包持有其单元格。此时绑定可以存放在帧槽中，单元格身份不可观察。

必须区分**绑定逃逸**和**值逃逸**：把对象写进未逃逸绑定不会让该对象变成局部值；闭包捕获的绑定也可能只保存原始值。本节的循环优化只依赖绑定逃逸，值逃逸不作为第一阶段的判定依据。

#### 循环头的每轮绑定

`for`、`for…in`、`for…of`、`for await…of` 头部的 `let` / `const` 在每轮迭代有独立绑定。独立单元格只有在“某轮创建的闭包能在该轮之后读取或写入它”时才可观察。未被任何闭包捕获的循环头绑定，每轮复制和单元格身份都不可观察。

| 绑定情形 | 存储 | 每轮环境 |
| --- | --- | --- |
| 循环头绑定，无嵌套函数读写 | 帧槽；跨挂起时按活性进入 `P`，否则为 `T` | 不复制；循环体没有被捕获的块级声明时不分配 |
| 循环头绑定，被嵌套函数读写 | 堆环境 | 复制到新的每轮环境 |
| 同一循环头混合上述两种 | 逃逸绑定在环境，未逃逸绑定在槽 | 只为逃逸绑定分配和复制 |
| 普通局部绑定 | 现有规则：不跨挂起为 `T`，跨挂起为 `P` | 不适用 |

变换规则：

- 未逃逸的循环头绑定提升为帧槽。`GetBinding` / `SetBinding` / `DeclareBinding` 改写为槽移动，每轮以普通写入更新同一槽；C 风格 `for` 在 `update` 前的第二次环境构造同样省略。
- `NewIterationEnv` 只列出仍然逃逸的绑定。若绑定集合为空且循环体内没有被嵌套函数读写的块级声明，不创建每轮环境，`SaveEnv` / `RestoreEnv` 不再承担可观察状态。若循环体内仍有被嵌套函数读写的块级声明，则保留空绑定表的每轮环境：它不复制任何循环头绑定，只为该声明的每次迭代提供新单元格。仅因提升失败而留在环境中的未捕获块级声明不需要新单元格，可使用函数环境。
- 环境省略不改变闭包捕获链的语义：闭包仍捕获创建时的当前环境；未逃逸绑定不在任何环境中，因此没有单元格身份可泄漏。
- `const` 的每轮初始化写入仍按编译期绑定规则允许；槽复用不得把它降级为对 `const` 的普通赋值并在运行期抛错。
- `break`、`continue`、`return`、`throw` 和 `finally` 只影响控制流；退出循环并恢复外层环境（仅当仍有逃逸绑定）后，未逃逸槽不再被闭包引用。
- 优化不改变现有 TDZ 规则。循环头绑定在每轮赋值前不可观察的前提正是无捕获；有捕获时保持现有每轮环境行为。

实现上，lowering 记录每个迭代环境组（保存槽、全部构造点、恢复点和绑定列表）；提升阶段把可提升的绑定从组中移除，组为空时由压缩 pass 删除整组并统一重映射跳转、异常表、恢复表与生成器表的 PC。也可以把空绑定 `NewIterationEnv` 定义为不分配环境，但这只是局部实现选择，不改变本节语义契约。

#### 与挂起、快照和 GC 的关系

- 未逃逸绑定是普通逻辑值，参与同一套活性分析：不跨挂起为瞬时槽，跨挂起进入固定持久槽，并按所在 `ResumeEntry` / `GeneratorEntry` 的 `saved_slots` 保存。恢复只重建当前迭代的活跃槽，不重跑已完成轮次。
- 被省略的每轮环境不进入快照，也不作为堆根；槽中的值仍按现有对象图、闭包环境和宿主槽规则遍历。保留的逃逸环境维持原有对象身份和引用关系。
- 优化结果必须是构建的一部分：分析是确定性的，生成的指令与槽布局进入 `build_id`；不得依据运行时值、GC 时序或宿主回调决定是否省略。
- 绑定级省略不改变指令格式时无需升 `BYTECODE_VERSION`；若实现改为删除指令、增加无操作指令或新的虚拟对象指令，则按字节码契约升级并同步加载校验。

#### 后续扩展（不属于第一阶段）

- **闭包逃逸**：若闭包只在同一次同步区域内创建和调用、不跨挂起、不存入堆或宿主、不参与身份比较，可以进一步省略其环境或直接内联。需要同时证明 `this`、`arguments`、`new.target`、`super` 和异常路径不受影响。
- **非逃逸对象/数组的标量替换**：需要独立的“值逃逸”分析，证明值不传给脚本或宿主、不写入堆对象或闭包、不跨挂起、不参与可观察的身份比较，并且所有属性访问都静态可枚举。它需要新的虚拟对象字节码表示与快照规则，不能与绑定级优化混为一谈。
- 不做 JIT、不做跨函数内联，也不以优化为由改变 GC、Promise 或快照的语义。

#### 验证

- 差分：同一 `.js` 用例分别以优化开/关构建执行，断言结果、副作用次数、异常与微任务次序一致。
- 捕获语义：未捕获的循环头绑定在 `await`、`break`、`continue`、`return`、`finally` 和快照恢复后行为不变；被闭包捕获的循环头绑定仍每轮独立，闭包身份和值不受优化影响。
- 编译期断言：未逃逸循环头绑定不出现在任何 `NewIterationEnv` 的绑定表中；逃逸绑定仍保留，混合循环只复制逃逸绑定。
- 加载与快照：加载器继续校验 `NewIterationEnv` 的绑定归属与槽范围；快照恢复时未逃逸绑定按帧槽、逃逸绑定按环境恢复，不出现悬空环境或重复根。

### 挂起与恢复协议

- `AWAIT` 将输入转为 VM 内可等待的 Promise，读取操作数后保存续体，按 JS 微任务顺序安排后续反应。即使 Promise 已结算，也不能在 `AWAIT` 原地同步执行续体。
- `YIELD` 不创建等待关系，也不经过微任务：生成器对象自身保存函数 ID、生命周期状态和按 `GeneratorEntry` 选出的持久槽，调用者的 `next(value)` 在不新建脚本帧的前提下同步恢复。async generator 的请求队列是 VM 堆对象，每个请求记录种类、参数和结果 Promise；请求由普通 Promise 反应驱动，不借用原生任务。
- 挂起帧的逻辑快照记录为 `(object_id, func_id, resume_id, result_promise_ref, slot_values[])`；最后一项按 `saved_slots` 顺序写值，无需逐槽写编号。被等待 Promise 的反应记录引用续体，等待关系不在帧中重复编码。VM 在当前微任务轮次排空后才允许快照，因此无需落盘已入队但尚未执行的恢复任务。
- 恢复入口面向受信且未经篡改的 VM 存档，精确匹配构建与当前宿主声明；独立的结构审计用于开发和诊断，不要求可信恢复重复遍历堆图。
- 恢复先核对版本与构建，再为堆、Promise 和续体重建身份，按恢复表重建挂起状态。每个挂起续体必须恰好对应一个未结算 Promise 上的等待反应，async generator 的每个排队请求必须恰好关联一个未结算的结果 Promise；诊断恢复检查这些镜像内部关系。原生绑定验证通过前不调度。未来 Promise 结算时，VM 重建帧，只填充指定的 `P` 槽，写入成功值或拒绝原因，从对应落点执行；不重跑 `AWAIT`、`YIELD` 或之前的副作用。
快照从模块环境、全局环境、绑定模块状态、显式脚本根及 pending operation 等持久根遍历对象图。绑定实例的槽按普通强边遍历；挂起续体、生成器对象及 async generator 请求队列只访问恢复表中的槽值与必要的运行时引用。从续体省略某个引用，不代表经模块、绑定实例或闭包仍可达的对象也可以省略。

### 两个挂起点的示例

```js
import { inspect, seed, wait } from 'demo';

export async function run(input) {
  inspect(input);
  const base = seed();
  const x = await wait("a");
  const sum = base + x;
  const y = await wait("b");
  return sum + y;
}
```

本例中 `inspect`、`seed` 和 `wait` 来自演示宿主；这里的 `wait` 在恢复后继续等待外部交付数值，区别于下文仅用作暂停点的 `test.wait`。`base` 固定为 `P[0]`，`sum` 固定为 `P[1]`，`x` 和 `y` 各自由成功入口写入瞬时槽。

| `resume_id` | `saved_slots` | 示例槽值（`seed()` 返回 `10`，`a` 返回 `2`） |
| --- | --- | --- |
| `0` / `await wait("a")` | `[0]` | `[10]` |
| `1` / `await wait("b")` | `[1]` | `[12]` |

`input` 不从续体保存，`base` 也不进入第二个快照。若从第二个快照恢复后 `b` 返回 `3`，成功入口只需写入 `y`，函数返回 `15`，不会再调用 `inspect`、`seed` 或 `wait("a")`。若 `b` 拒绝，则进入拒绝落点并拒绝 `run` 的 Promise。

## 暂停与快照边界

只在同步执行栈已清空、当前微任务轮次排空后创建快照。同步调用和原生函数执行中的状态不保证可暂停或恢复。此边界避免序列化宿主栈及半途执行的原生调用。生成器只在控制权已返回脚本并挂起于 `yield` 时进入快照；执行中的生成器以及重新进入的 `next` / `throw` / `return` 属于同步执行栈，不提供暂停。

待触发的宿主 timer 和已进入 VM 的普通任务都不是已入队的 VM 微任务，可以跨快照存在。宿主的切点须协调事件交付与存档：不接受已从宿主摘下、却未写入 VM 且无法由持久记录重建的投递。恢复发布前不得开始外部工作或投递；不保存正在运行的调度调用栈。

### Operation 的中断与恢复

异步绑定返回 VM Promise，并建立该次操作的持久记录。每个 operation 类型必须声明从任一 pending 阶段被中断后的恢复策略；没有策略的类型不能注册，也不能启动这类工作。允许正常快照不等于要求原生任务可续跑；宿主根据该类型的配置和快照内的状态决定原 Promise 的结果：

| 操作语义 | 恢复后的动作 |
| --- | --- |
| `test.wait` 只用来制造暂停点 | 静默成功，以 `undefined` 兑现原 Promise，不再等待一次外部交付。 |
| timer 或等待外部交付的操作 | 根据快照状态重建登记；到期或收到交付时通知原任务或结算原 Promise。 |
| `fetch` 的外部传输 | 不恢复旧传输；按 Fetch 所处阶段拒绝原 Promise 或使未结束的体流出错，不重发请求。 |

ID 只用于关联交付与 pending 记录，不是去另一份宿主存档查询数据的键。只有整份快照恢复成功后才能重新登记、重发或结算；旧任务的迟到结果不能再次结算。按策略拒绝 Promise 是有效的恢复结果，经原来的反应和异常路径交付，不是 VM 恢复失败。恢复不重跑发起该 operation 的函数。

## 快照数据

快照至少应包含：

- VM 快照格式版本、字节码版本、脚本构建 ID，以及所需静态绑定的声明标识和契约版本（含槽布局、payload codec 和 operation 策略）。
- VM 堆对象图与对象间引用关系、入口模块的顶层环境（独立于全局环境），包括导出绑定的当前值。恢复后从同一构建的导出表定位恢复后的绑定，不重复序列化导出值。
- 可达 BigInt 原始值，包括脚本槽、续体、Promise 结果和 operation 交付；按规范十进制字符串加专用标记编码，恢复时校验符号、数字与无前导零形式。
- 可达的 VM Promise、挂起续体和等待关系；局部状态只包含当前恢复表指定的持久槽。
- 可达生成器对象的函数 ID、生命周期状态、挂起恢复入口和该入口的持久槽；async generator 的请求队列按入队顺序保存请求种类、参数、结果 Promise 及下一个待处理位置。生成器对象按普通堆对象保留身份和引用关系，不保存原生迭代器或宿主生成器。
- 绑定模块状态和可达绑定实例：对象身份、普通属性、实际原型引用、类型标识、JS 值槽与可选 payload 的版本和编码内容。绑定函数对象保存稳定声明标识，调用实现由本进程提供。
- 每个 pending operation 的类型及版本、实例 ID、恢复策略所需的完整状态（JS 引用或编码后的非 JS 数据），以及原 VM Promise 或任务终点的关联。
- VM 相对时钟的累计时间、已纳入 VM 的普通任务及顺序，以及其所引用的定时器、流和其他 Web API 状态。排队任务引用 VM 对象或有类型的持久数据，不引用宿主回调。

快照不包含机器码、原生指针、OS 句柄、原生回调或正在执行的任务。JS 回调是 VM 函数对象，其引用和捕获环境仍在对象图中。恢复无需旧 host 内存、实例索引表或第二份 operation/UI 绑定存档。

## 绑定恢复协议

1. 校验格式、Build 和快照引用的静态声明及版本，链接函数实现、codec 和 operation 策略。注册表不提供任何旧实例。
2. 先为堆对象、Promise 和续体建立身份，再解析并校验原型、槽、等待关系、生成器恢复状态及模块状态。恢复已保存的原型对象与属性，不用静态模板覆盖脚本所做的修改。
3. 在未对外可用的 VM 中用 codec 解码非 JS 数据，准备所有 pending 记录。声明缺失、槽布局或引用非法、数据无法解码，均整体拒绝恢复；此时不执行脚本、不重发请求，也不投递事件。
4. 所有记录就绪后开放交付，按各 operation 策略恢复工作或结算原 Promise。脚本反应遵循正常微任务顺序；不重跑模块初始化、工厂函数或挂载副作用。

## 版本与失败策略

快照格式、字节码版本、脚本构建 ID 或所需绑定契约版本任一不匹配时，拒绝恢复并报告不匹配项；不承诺自动迁移旧快照。宿主可按应用策略另行初始化脚本并重建 UI，但不得把这一路径当作成功恢复。

## 主要风险与待实现时验证事项

- 框架升级或打包器改动可能引入新的语法、内建或宿主依赖；升级时须重新检查实际构建产物并通过上述运行及恢复验收，不能仅凭入口不变宣称兼容。
- async 帧、生成器挂起状态、闭包环境和对象图的序列化必须保留引用关系与对象身份。
- 区分恢复校验和外部副作用：损坏的绑定或 operation 记录必须整体拒绝且不重发请求；合法的“恢复即拒绝”只拒绝对应 Promise，不阻碍其他续体运行。
- 精确版本策略简单可靠，但脚本升级后旧快照不可直接恢复；宿主需实现重新初始化与 UI 重建路径。
- 实现阶段应为快照往返、挂起 async 帧恢复、对象身份保持、缺失绑定拒绝执行及版本不匹配拒绝恢复设计针对性验证。
- 销毁原 VM 和宿主后，只用快照、同一 Build、静态绑定和平台服务恢复：保存 `m = expect(obj)`、`obj.matcher = m` 和 `f = m.toEqual.bind(m)`，恢复后经别名修改 `obj` 再调用 `f`。同时验证实例共享方法、裸调抛错、不可达实例不要求解码；另用带 payload 的节点验证上下文解码，不依赖旧节点表。
- 对每种 operation 策略验证原目标的结果及交付次数：`test.wait` 静默成功；继续等待的操作只接受一次完成；Fetch 恢复按阶段中断且不发出请求。其他声明为可重发的自定义 operation 不重跑发起函数。
- 验证无需 `"use strict"` 即按严格 ESM 执行：顶层 `this` 为 `undefined`，顶层声明不成为全局对象属性，且同名模块绑定不会修改原全局值。
- 验证宿主只能通过公开名读取或调用显式导出，包括别名；未导出的函数不能调用，非可调用导出可以读取但不能调用。脚本更新导出绑定后，宿主读到新值、调用新函数；恢复后仍观察到更新后的绑定，且顶层初始化不重跑。编译器拒绝重复导出名与不存在的局部绑定。
- 验证生成器跨快照的挂起与恢复：在 `yield` 挂起时保存，恢复后分别执行 `next` / `throw` / `return`，断言结果对象的 `done` / `value`、`finally` 完成状态与生成器身份一致；生成器执行中重入抛出 `TypeError`；`yield*` 委托在恢复后继续向同一迭代器转发。
- 验证 async generator 的请求队列：在 `await` 与 `yield` 边界保存，恢复后按入队顺序逐个结算，每个请求只结算一次，`return` 请求在已排队请求处理完后才完成生成器；`for await…of` 在 `break`、异常退出及恢复后只调用一次异步 `return()`，收尾异常不覆盖原完成状态。
- 对同一函数的不同挂起点断言实际写出的槽和值；验证不从快照保存只在瞬时槽可达的对象，同时保留由闭包或全局别名引用的对象。对生成器函数分别覆盖 `next` 的正常落点、`throw` 的异常落点与 `return` 的完成落点，断言三条入口的 `saved_slots` 与恢复落点。以未快照运行作为对照，比较结果、宿主 operation 完成次序及副作用次数。
- 覆盖仅拒绝路径使用的槽、`finally` 内二次挂起时的待完成状态、已结算 Promise 仍通过微任务恢复，以及续体释放后无多余强引用。
- 验证加载器拒绝缺失活跃槽、越界恢复点和非法落点，快照恢复器拒绝槽数量不符或 Promise/续体关联损坏，均不得启动 VM。
- BigInt 的任意精度表示、混合类型比较/相等和 `Number` 转换边界容易偏离 ECMAScript；构建常量、快照、宿主槽与 operation 交付都不得经过浮点或 JSON 数值中转。
- 验证 BigInt 字面量、算术与位运算、混合类型 `==` / `===` 的差异、除法向零截断、负指数 `RangeError` 和 `JSON.stringify` 抛错；把大整数写入槽、跨 `await` 保存并在新 VM 恢复，断言值逐位一致、`typeof` 与包装对象行为不变。
- 验证逃逸分析不改变可观察语义：未捕获的循环头绑定省略每轮环境后，在 `await` 前后、`break` / `continue` / `return` / `finally` 与快照恢复中结果一致；被闭包捕获的绑定仍每轮独立。编译期断言未逃逸绑定不出现在 `NewIterationEnv`，逃逸绑定仍保留；同一用例在优化开/关构建下的结果、副作用次数与微任务次序相同。

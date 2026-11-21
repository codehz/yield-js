# JS 基础语法特性实现缺口（现状盘点）

> 基线：`docs/design.md` 的「首版 JavaScript 支持范围」，对照当前 `crates/compiler` + `crates/vm`。
> 已实现范围见 [`implemented.md`](implemented.md)，语法子集与缺口对照本文。
> 语法子集拒绝通常报告带行列的 `unsupported syntax: …`；这与无效 JavaScript 的语法诊断不同。运行期的语义偏差另列；不把宿主或 VM 故障当作脚本异常的缺口。

## 1. 值、转换与全局名

`Value` 仍是 `Undefined / Null / Bool / Number(f64) / String / Symbol(SymbolId) / Object(ObjectId)`。`RegExp` 实例与显式包装对象通过堆对象表示，不是新的 `Value` 变体。

- 尚未提供部分 well-known Symbol（如 `Symbol.asyncIterator`）及其协议。`JSON.stringify` 查找字符串键 `toJSON`；标准没有 `Symbol.toJSON`。
- 名称策略保留一个偏差：没有同名 preload 导出时，`typeof` 对被局部绑定遮蔽的宿主导入或顶层函数仍可能折叠为 `"function"`（`names.rs`）。

## 2. 合法输入的编译期限制

- 正则字面量由 Oxc 解析并由 VM 的正则匹配器校验；匹配器不支持的模式会在编译期报告带行列的 `unsupported syntax: …`，而无效模式或标志也可能先被 Oxc 拒绝。匹配器范围见第 4 节。
- 同一函数内对尚未初始化的词法绑定的部分访问（如 `x; let x;`）在编译期拒绝，而不是运行时抛出可捕获的 `ReferenceError`。更完整的 TDZ 不在首版范围内。
- 首版明确排除的合法语法见第 6 节；无效 JavaScript 示例见第 7 节。

## 3. 能编过，但语义不完整

| 语义点 | 现状 |
| --- | --- |
| `queueMicrotask` / Promise 拒绝 | `queueMicrotask` 回调抛出的异常通过 `Host::report_error` 交给宿主；未处理的 Promise 拒绝仍不跟踪 |

## 4. 内建 API 的限制与缺项

下表列举当前限制，不是首版承诺或完整的 ECMAScript 兼容性清单。已提供的全局与方法见 [`implemented.md`](implemented.md)。

| 内建 | 限制与缺项 |
| --- | --- |
| `Function` | `Function` 可反射，但调用和构造均抛出 `TypeError`，不编译字符串；`toString` 返回带初始名字的占位文本，不暴露源码 |
| `Array` | 缺少 `at`、`sort`、`reverse`、`copyWithin`、`findLast` / `findLastIndex`、`keys` / `entries` 等方法。`map` 等多数原型方法只接受数组接收者。不支持 species、自定义 `from` / `of` 构造器或 `concat` 展开协议 |
| 错误构造器 | 缺少专用的 `Error.prototype.toString`；`stack` 是简化文本，不包含调用帧 |
| `Number` | 非十进制 `toString` 只转换整数部分；`toLocaleString` 仅沿用默认十进制 `toString`，不支持区域设置 |
| `Math` | 浮点数学函数依赖 Rust `f64` 实现；`random` 使用固定种子的非加密 PRNG |
| `RegExp` | 匹配语法取决于固定版本的 `regress`；支持 `d` / `g` / `i` / `m` / `s` / `u` / `v` / `y`（`u` 与 `v` 互斥），不保证覆盖全部 Annex B 兼容语法。字符串方法仍查找相应的 Symbol 协议，但内建 `[Symbol.matchAll]` / `[Symbol.split]` 克隆匹配器时不使用 species；直接调用内建 `[Symbol.matchAll]` 也要求全局标志 |
| `Map` / `Set` | 已覆盖基础集合操作和同步迭代；不提供较新的 `Map.groupBy` 或 `Set.prototype.union` 等集合运算 |
| `Proxy` | 目标限于普通对象、数组、原始值包装对象及最终指向它们的代理；仅提供 `get` / `set` / `has` / `deleteProperty` / `ownKeys` / `getOwnPropertyDescriptor` 六种 trap。基线之外的代理特性见第 6 节 |
| `Promise` | `then` 和 `finally` 支持 species；`catch` 和 `finally` 会查找接收者的 `then`，但仍不能借给非原生 Promise 对象。静态方法仍只接受内建 `Promise` 接收者；组合子会读取其 `resolve` 处理输入，但不会用自定义构造器创建结果。 |

## 5. 结构性缺口

1. **代理仅覆盖六种 trap**，不等同于完整 ECMAScript Proxy。
2. **新的堆种类仍要改可达性**。常规强边由 `Heap::write_edges` 提供；宿主实例的隐藏槽和模块状态槽由 GC 和快照共享的追踪器显式遍历，`WeakMap` 条目由 ephemeron 追踪单独处理，`WeakSet` 成员不参与标记。快照布局变化调整 `SNAPSHOT_FORMAT_VERSION`；仅执行语义变化可更新构建摘要的语义修订号来拒绝旧快照，不必因此调整 `BYTECODE_VERSION`。
3. **宿主绑定首版仍有未决项。** 尚未实现弱引用槽、`JsMap` 槽、C ABI、TypeScript 定义生成、payload 流式/资源上限和跨线程交付。宿主绑定与脚本继承的已支持范围见 [`implemented.md`](implemented.md)。
4. **宿主续体不能等待任意 Rust Future。** `loop` / `while` / `while let` 的循环体可挂起；`while` / `while let` 的条件和 `for` 循环不能挂起。Guest 调用与 GC 同步恢复；只有正在等待外部 operation 的宿主任务可写入快照。Rust 栈和函数指针不序列化。
5. **GC 不会自动触发。** 宿主可在调用之间排空微任务后调用 `Vm::collect_garbage`，或选择用 `CallCx::request_gc` 向脚本暴露同步收集；后者通过 `Host::gc_roots()` 提供宿主保留的本 VM 值。没有默认的脚本 `gc` 全局函数。

## 6. 首版范围之外（不是首版缺口）

- 完整 ECMAScript TDZ、每轮循环独立绑定
- generator、async generator、`for await…of`、顶层 `await`、动态 `import()`、`eval`、`Function` 构造
- 标签模板：仍由离线构建降级。`class` 由 VM 原生执行，不再离线降级。
- 严格 ESM 支持本地命名导出（含本地宿主导入绑定）和清单中的静态宿主模块；不查找或加载 JS 文件。`import.meta`、导入属性、默认导出和带模块来源的再导出在编译期拒绝。
- `debugger`、`using` / `await using`：不在首版语法表
- `BigInt`：design 未列入
- `WeakRef`、`FinalizationRegistry`、共享内存、`Atomics`
- 可撤销代理、代理函数/构造器，以及 design 未点名的其它 Proxy traps
- 旧式 `Object.prototype.__proto__` 访问器；保留 `Object` / `Reflect` 的显式原型操作。
- 快照迁移：恢复要求 `SNAPSHOT_FORMAT_VERSION`、`BYTECODE_VERSION`、`build_id` 和宿主绑定指纹均匹配。

## 7. 无效 JavaScript（拒绝不是实现缺口）

以下写法违反 JavaScript 或严格模式规则，即使诊断由 lowering 报出，也不表示缺少对应特性的实现。

- `for…in` / `for…of` 循环头的变量声明只能有一个声明项（其中可以解构多个绑定），且在严格 ESM 中不能带初始化器。
- 数组、对象解构赋值模式只能配 `=`；复合及逻辑赋值要求简单赋值目标，自增减也要求有效的赋值目标。
- 模块顶层及仅嵌套在其中的箭头函数不能使用 `new.target`。普通函数有自己的 `new.target`，嵌套箭头函数可捕获它。
- 同一个 `switch` 中重复 `default`，以及默认导出以外的匿名函数声明，都是语法错误。合法的 `export default function () {}` 不在此例之内，但该子集不支持默认导出。
- 严格模式下不能使用 `with`，也不能删除裸标识符（包括加括号的标识符）。

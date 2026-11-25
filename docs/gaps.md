# JS 基础语法特性实现缺口（现状盘点）

> 基线：[`design/language.md`](design/language.md) 的「首版语言与运行时语义」及 [`design/build.md`](design/build.md) 的构建输入，对照当前 `crates/compiler` + `crates/vm`。
> 已实现范围见 [`implemented.md`](implemented.md)，语法子集与缺口对照本文。
> 语法子集拒绝通常报告带行列的 `unsupported syntax: …`；这与无效 JavaScript 的语法诊断不同。运行期的语义偏差另列；不把宿主或 VM 故障当作脚本异常的缺口。

## 1. 值、转换与全局名

`Value` 仍是 `Undefined / Null / Bool / Number(f64) / BigInt(Arc<BigInt>) / String / Symbol(SymbolId) / Object(ObjectId)`。`BigInt` 的任意精度内容由 Arc 持有，不是新的堆对象；`RegExp` 实例与显式包装对象通过堆对象表示，也不是新的 `Value` 变体。

- `Symbol.asyncIterator` 作为保留 well-known Symbol 提供，由 async generator 与 `for await…of` 消费；`for await…of` 由编译器展开，同步回退不分配可反射的 `AsyncFromSyncIterator` 包装器（也没有其它内建异步迭代器），并且只对每轮的 `value` 做隐式 `await`。`JSON.stringify` 查找字符串键 `toJSON`；标准没有 `Symbol.toJSON`。

## 2. 合法输入的编译期限制

- 正则字面量由 Oxc 解析并由 VM 的正则匹配器校验；匹配器不支持的模式会在编译期报告带行列的 `unsupported syntax: …`，而无效模式或标志也可能先被 Oxc 拒绝。匹配器范围见第 4 节。
- 同一函数内对尚未初始化的词法绑定的部分访问（如 `x; let x;`）在编译期拒绝，而不是运行时抛出可捕获的 `ReferenceError`。更完整的 TDZ 不在首版范围内。
- `yield` / `yield*` 出现在非生成器函数中会被拒绝。
- 模块初始化帧是同步的：顶层支持 `using`，但拒绝 `await` / `await using`，嵌套块也不能绕过该限制。
- 默认文件系统解析仅支持 ESM；显式 Web 生产解析仅支持静态 CommonJS 子集，不支持动态 `require`、`require` 逃逸、`require` ESM 或无法静态确定的导出形状。两种模式均不支持 JSON 模块、Node 内建、`module` 回退、`exports` 通配符或数组、跨安装边界的符号链接。
- JSX 只在 `.jsx` 模块中启用；不支持 JSX 命名空间或 spread 子节点。
- 自定义 AST 转换依赖固定版本的 Oxc，不提供跨 Oxc 版本的兼容层。图转换引入的源码依赖必须已在图中；需要发现新模块的转换应在源码阶段运行。
- 首版明确排除的合法语法见第 6 节；无效 JavaScript 示例见第 7 节。

Rust 宿主不支持运行时增删或替换服务。`host!` 构造时确定布局；声明了必需服务但没有注册的宿主会在链接时被拒绝。

## 3. 能编过，但语义不完整

当前未列出此类独立缺口；内建限制见下节。

## 4. 内建 API 的限制与缺项

下表列举当前限制，不是首版承诺或完整的 ECMAScript 兼容性清单。已提供的全局与方法见 [`implemented.md`](implemented.md)。

| 内建 | 限制与缺项 |
| --- | --- |
| `Array` | 不提供 `Array.prototype.toLocaleString`；`flat` 在无法减少深度的循环数组上抛 `TypeError`，而不是无限展开 |
| 错误构造器 | `stack` 只包含创建时仍在执行的同步脚本帧，不包含 Rust 宿主帧，也不追踪 `await`、`yield` 或微任务之前的调用历史 |
| `DOMException` | 提供 Web IDL 异常对象和历史常量，不因此提供浏览器 DOM API |
| 事件与取消 | 不提供 `Node`、`ShadowRoot`、activation 或 retargeting。全局错误位置仅来自报告时可见的同步脚本帧。 |
| 定时器与时钟 | 不支持字符串 timer handler、Performance Timeline 或 User Timing。`performance.timeOrigin` 只是 VM 相对时间域的零点，不是 epoch；全局 timer 和 `AbortSignal.timeout` 需要宿主唤醒服务。 |
| `navigator` | 只提供 `userAgent` 与 `platform` 两个属性；没有 `Navigator` 原型、语言、硬件能力、在线状态等其它 HTML 成员。`platform` 是编译目标 `<target_os>-<target_arch>`，不是浏览器平台字符串。 |
| Web Crypto | 支持随机方法及 `crypto.subtle` 的 SHA-256、raw 密钥导入/导出、PBKDF2-SHA-256 派生 AES-GCM、AES-GCM 加解密、HMAC-SHA-256 签名/校验子集。无全局 `Crypto` 接口对象，无不可导出密钥（因此 PBKDF2 只接受本实现的可导出 raw 密码），无其他格式、算法或用途；不提供 `generateKey`、`deriveBits`、`wrapKey`、`unwrapKey`。 |
| `Number` | 非十进制 `toString` 只转换整数部分；`toLocaleString` 仅沿用默认十进制 `toString`，不支持区域设置 |
| `BigInt` | `toLocaleString` 与十进制 `toString` 相同，不支持区域设置；其余语义见 [`implemented.md`](implemented.md) |
| `Date` | 不提供 Intl；`toLocale*` 复用本地日期或时间格式。`parse` 支持标准 Date Time String Format 和自身 `toString` / `toUTCString` 的输出，不接受其它非标准格式。`GMT±HHMM` 不含偏移秒数；解析这种历史本地字符串时借助当前宿主时区恢复偏移秒数。未注册 `Arc<dyn TimeService>` 时使用固定 epoch 0 与 `Etc/UTC` 的 dummy 时间源 |
| `Math` | 浮点数学函数依赖 Rust `f64` 实现；`random` 使用固定种子的非加密 PRNG |
| `RegExp` | 匹配语法取决于固定版本的 `regress`；支持 `d` / `g` / `i` / `m` / `s` / `u` / `v` / `y`（`u` 与 `v` 互斥）。不实现 Annex B 的 `compile` 和旧式静态匹配属性，不保证覆盖全部 Annex B 兼容语法。 |
| `Map` / `Set` | 查找、删除与集合运算按活槽线性扫描，删除保留空槽 |
| `Proxy` | 目标限于普通对象、数组、原始值包装对象、Date、DOMException、ArrayBuffer、TypedArray、DataView 及最终指向它们的代理；仅提供 `get` / `set` / `has` / `deleteProperty` / `ownKeys` / `getOwnPropertyDescriptor` 六种 trap。基线之外的代理特性见第 6 节 |
| `ArrayBuffer` / `DataView` / TypedArray | 缓冲区、11 种 `DataView` 宽度、12 个 TypedArray 构造器及其原型方法按 ES2026 实现，含可调整长度缓冲区与 base64/hex 编解码。`toLocaleString` 复用 `Number.prototype.toLocaleString` 与 `BigInt.prototype.toLocaleString`，没有 Intl 文本（与 `Number` / `BigInt` 的限制相同）。`SharedArrayBuffer`、`Atomics`、分离后重新附加不在范围内 |
| `TextEncoder` / `TextDecoder` | `TextEncoder` 输出 UTF-8；`TextDecoder` 支持 UTF-8、UTF-16LE、UTF-16BE 及其标准标签。其他标签（如 `gbk`）按设计不支持，抛 `RangeError`。`TextEncoderStream` / `TextDecoderStream` 复用同一编码规则与增量状态。 |
| Streams | 已提供排队策略、Readable/Writable/Transform、字节读取、pipe/tee、async iterator/`from`、文本转换流、流转移与压缩转换流。 |
| `URL` / `URLSearchParams` / `URLPattern` | 尚未运行完整上游 WPT；对象 URL 表仅在本 VM 内可见 |
| Fetch 与二进制对象 | 已提供 `Blob`、`File`、`FormData`、`Headers`、`Request`、`Response` 和 `fetch`；`FormData` 不读取 HTML 表单。没有 TLS、代理、cookie jar、HTTP 缓存或预检结果缓存（每次跨源非简单请求重新预检）；示例适配器只允许显式配置的 `127.0.0.1` 明文 HTTP/1.1，HTTPS 需其他适配器实现。`keepalive` 不跨 VM 销毁或快照恢复继续传输；恢复中的在途传输按设计中断且不重发，`only-if-cached` 因无缓存总是 miss |
| `structuredClone` | 可转移 `ArrayBuffer`、`ReadableStream`、`WritableStream` 与 `TransformStream`（流仅限同一 VM 内转移）；不克隆函数、代理、模块命名空间、宿主实例等不支持的种类，不复制 VM 执行环境 |

`yield-blitz` 首版提供 React DOM 首屏挂载所需的 document、element、text 树读取与修改、原生宿主类身份、样式 expando 和 VM `EventTarget`；没有完整浏览器 DOM、原生输入事件、样式计算 API、点击更新、热更新或 VM/DOM 快照恢复。无窗口测试通过不等于 Vello 目视验收通过。
首屏路径只编译 `src/blitz.jsx` 及其 npm 依赖，不验收 `src/index.tsx`、TypeScript 类型剥离、CSS 导入或动态样式模块。

未提供的全局名包括 `WeakRef`、`FinalizationRegistry`、`SharedArrayBuffer`、`Atomics` 和 `Intl`。`Temporal` 与 `Iterator.zip` / `Iterator.zipKeyed` 等新增迭代器方法不属于 ES2026。

## 5. 结构性缺口

1. **代理仅覆盖六种 trap**，不等同于完整 ECMAScript Proxy。
2. **新的堆种类仍要改可达性**。常规强边由 `Heap::write_edges` 提供；宿主实例的隐藏槽和模块状态槽由 GC 和快照共享的追踪器显式遍历，`WeakMap` 条目由 ephemeron 追踪单独处理，`WeakSet` 成员不参与标记。快照布局变化调整 `SNAPSHOT_FORMAT_VERSION`；仅执行语义变化可更新构建摘要的语义修订号来拒绝旧快照，不必因此调整 `BYTECODE_VERSION`。
3. **宿主绑定首版仍有未决项。** 尚未实现弱引用槽、`JsMap` 槽、C ABI、TypeScript 定义生成、payload 流式/资源上限和跨线程交付。非空模块需显式选择初始化及 codec；可为普通 Rust 字段派生 `Encode` / `Decode`。宿主绑定与脚本继承的已支持范围见 [`implemented.md`](implemented.md)。
   宿主模块的类、operation 与必需服务类型必须显式列出，不从导出签名或函数体推断；清单也覆盖继承、间接调用及恢复路径。可选服务不列入模块依赖，由调用方处理缺失。依赖声明只做链接期检查，不限制调用时的服务访问。
4. **宿主续体不能等待任意 Rust Future。** `loop` / `while` / `while let` 的循环体可挂起；`while` / `while let` 的条件和 `for` 循环不能挂起。Guest 调用与 GC 同步恢复；只有正在等待外部 operation 的宿主任务可写入快照。Rust 栈、函数指针和 `ModuleCx` 不序列化；模块续体只在分派时重获本模块的状态能力。
5. **GC 不会自动触发。** 嵌入方可在调用之间排空微任务后调用 `vm.collect_garbage(roots)`，或用 `CallCx::request_gc` 向脚本暴露同步收集。只有后者使用可选的 `Rc<dyn GcRoots>` 服务访问宿主持有的本 VM 值。没有默认的脚本 `gc` 全局函数。
6. **瞬时槽复用不是精确 GC。** 分配器使用覆盖所有活性位置与写入点的连续区间，不利用区间内部的空洞。活动帧的 GC 仍扫描所有非空槽，未被覆盖的死引用可能继续保留对象。
7. **静态属性键不缓存读取。** 专用读取指令只跳过字符串键的槽读写和转换；`SetMember` 对普通对象的自有可写数据属性有写侧单态缓存，但读取、计算键、复合赋值和 `super` 仍走通用属性路径。
8. **CFG 常量传播不折叠通用 JS 运算。** 只传播已有字面量和槽复制、判断已知字面量的真假值；不做跨函数常量传播或运行时类型专用化。
9. **快照不是不可信导入格式。** `Vm::restore` 默认使用 `SnapshotValidation::Trusted`，不重查受信镜像的堆图；应用负责来源、完整性和读入大小。`RestoreOptions::validation` 可设为 `SnapshotValidation::Full` 以诊断镜像不变量，但不提供认证、隔离或资源预算。

## 6. 首版范围之外（不是首版缺口）

- 完整 ECMAScript TDZ
- 不支持运行时加载或编译代码。动态 `import()` 只接受字符串字面量，按依赖图提前求值，不保留惰性加载或原生动态导入时序。内建 `eval` 无论输入为何均抛 `EvalError`，调用或构造内建 `Function` 均抛 `TypeError`。
- `import.meta` 和导入/导出属性仍在编译期拒绝。跨模块读取有运行时 TDZ 检查，不改变普通函数的 TDZ 规则。
- `debugger`：不在首版语法表
- `WeakRef`、`FinalizationRegistry`、共享内存、`Atomics`
- 代理函数/构造器，以及 design 未点名的其它 Proxy traps；`Proxy.revocable` 与 `new Proxy` 共享当前目标和 trap 限制。
- 旧式 `Object.prototype.__proto__` 访问器；保留 `Object` / `Reflect` 的显式原型操作。
- 快照迁移：恢复要求 `SNAPSHOT_FORMAT_VERSION`、`BYTECODE_VERSION`、`build_id` 和宿主绑定指纹均匹配；二进制镜像还要求当前 `SNAPSHOT_BINARY_VERSION`，不做容器迁移。
- 产物迁移：`Build::from_bytes` 只接受当前 `BUILD_FORMAT_VERSION` 与当前 bincode 编码，不读取旧 JSON 产物。

## 7. 无效 JavaScript（拒绝不是实现缺口）

以下写法违反 JavaScript 或严格模式规则，即使诊断由 lowering 报出，也不表示缺少对应特性的实现。

- `for…in` / `for…of` 循环头的变量声明只能有一个声明项（其中可以解构多个绑定），且在严格 ESM 中不能带初始化器。
- 数组、对象解构赋值模式只能配 `=`；复合及逻辑赋值要求简单赋值目标，自增减也要求有效的赋值目标。
- 模块顶层及仅嵌套在其中的箭头函数不能使用 `new.target`。普通函数有自己的 `new.target`，嵌套箭头函数可捕获它。
- 同一个 `switch` 中重复 `default`，以及默认导出以外的匿名函数声明，都是语法错误。
- 严格模式下不能使用 `with`（不影响 `Symbol.unscopables` 和数组名单的反射），也不能删除裸标识符（包括加括号的标识符）。

## 8. 真实源码兼容性探针

`crates/harness/tests/e2e/react_compat.rs` 用真实 React 19（19.2.0，esm.sh 打包的单文件 ESM，来源见 `tests/e2e/vendor/react-19.2.0-esm-sh.mjs` 头部注释）验证严格 ESM 子集与宿主边界：编译探针分别在带 preload 与不带 preload 时编译整个生产 bundle。调度器恢复探针使用 `scheduler@0.27.0` 的生产 bundle（来源见 `tests/e2e/vendor/scheduler-0.27.0.mjs` 头部注释），在挂起后恢复并断言延迟回调只执行一次、已取消的回调不执行。

preload 可选地提供值为 `undefined` 的 `window` 与 `process`。`console`、`reportError`、`performance`、`setTimeout` 与 `clearTimeout` 由 VM 提供，原 scheduler 不使用 timer shim。`console.dirxml` 没有 DOM 树可展示，改用普通值预览；`%c` 样式随事件交给宿主，不在 VM 内渲染。

探针同时关闭了以下框架缺口：

| 缺口 | 现状 |
| --- | --- |
| `Object.getOwnPropertyNames` / `Object.getOwnPropertySymbols` 缺失，esm.sh 的 CJS interop 读取导出失败 | 两项已实现，按自有键顺序过滤字符串键或 Symbol 键 |
| async generator 函数缺少 `%AsyncGeneratorFunction.prototype%`，`Object.getPrototypeOf(Object.getPrototypeOf(async function*(){}).prototype)` 读到 `undefined` | 生成器与 async generator 函数现在各有函数原型，其 `.prototype` 指向共享实例原型 |
| `for (let …)` / `for (const …)` 循环头没有每轮独立的 `let` / `const` 绑定，循环内创建的闭包共享最后一次迭代的值 | C 风格、`for…in`、`for…of`、`for await…of` 均按每轮环境执行 |
| `Symbol.for` / `Symbol.keyFor` 被提取后裸调用会检查 `this` 并抛出 `TypeError` | 两个静态方法不再读取 `this`，与真实引擎一致 |

`MessageChannel` 的 `typeof` 条件所选死侧不再产生引用；`__REACT_DEVTOOLS_GLOBAL_HOOK__` 的缺名探测本身也不要求 preload 提供该全局。额外 e2e 编译探针不使用 preload，直接编译整条 React 图。React DOM 的实际挂载与节点树验收由 `yield-blitz` 承担。

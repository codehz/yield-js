# 宿主接口与静态绑定模型

> 本文是[总体设计](../design.md)的专题文档：C ABI 边界、静态模块/类声明、导出可见性与 JS 值槽。完整绑定契约、记录编码与宏细节见 [host-bindings.md](host-bindings.md)，VM 端的快照与恢复规则见 [snapshot.md](snapshot.md)。

## 宿主接口

详细展开（非最终草案）见 [`host-bindings.md`](host-bindings.md)。

- 提供稳定的 C ABI 宿主接口，并管理 VM 句柄的创建、恢复、执行与释放。
- 模块初始化完成后，宿主可按公开导出名读取当前值或调用当前可调用值；公开名可不同于局部名。导出是活绑定，不缓存初始化时的值，也不给宿主提供直接改写绑定的接口。未知导出名和调用不可调用值均报错，不回退到查找顶层声明或全局对象。

### Rust 宿主所有权

`Vm::new(build, host, options)` 与 `Vm::restore(snapshot, build, host, options)` 消费具体的、不可克隆的 `Host`；`Vm` 不以宿主类型为参数。执行和投递始终使用该 VM 的宿主，调用方不再传入或替换宿主。宿主及其服务实例不随快照保存；分叉恢复时为每个 VM 构造宿主，可显式共享允许共享的服务句柄。

`host!` 既可用 `dts`、`modules`、`services` 注册宿主绑定，也可单独使用 `services`；没有绑定或服务时使用 `Host::default()`。服务按确切的存储类型注册，嵌入方用 `vm.bind_service::<T>()` 解析并只读借用，回调通过声明的 scope 访问预绑定服务。外部工作可在后台执行，再由嵌入方回到 VM 线程投递代际令牌和结果；回调不得重入 VM。

可选服务 `Rc<dyn Diagnostics>` 通过 `report(&self, event: Diagnostic<'_>)` 接收 `Uncaught`（异常值与诊断文本）、`UnhandledRejection`（Promise 与理由）和 `StartupException`（诊断文本）。全局异常和未处理拒绝先派发可取消的脚本事件；仅未取消的报告进入服务，错误报告期间再次抛出的异常直接报告，不递归派发。模块初始化异常在返回原错误前报告，不派发脚本事件。未安装服务时没有宿主报告，脚本通知与错误返回不变。诊断携带的临时值不能脱离调用保存。`UserAgent(String)` 仅在新建时用于初始化 `navigator.userAgent`；恢复用快照中的属性。

可选服务 `Rc<dyn GcRoots>` 的 `visit(&self, visit: &mut dyn FnMut(&Value))` 在绑定请求同步 GC 时借出宿主持有的同 VM 值。显式收集与快照各自接收根切片，不调用该服务。

## 静态模块与原型

每份模块描述声明模块名、导出函数及其签名、类的槽布局与方法、可选的构造器导出，以及所用 codec 和 operation 类型。编译器从描述中取得导入清单，VM 装载时把声明标识链接到注册的实现。函数和方法都是一等 JS 函数值；读属性走普通原型链，调用时传入实际 `this`。宿主不用实现 `call_native`、按名字读取原生属性，或为每个实例制造方法函数。

下面以 `test` 模块的 `expect` 导出为例：`Matcher` 与模块类型一起静态注册，只有工厂函数向 JS 返回其实例。Rust 宏示例：

```rust
#[host_module(name = "test", classes = [Matcher])]
struct TestModule;

#[module_exports]
impl TestModule {
    #[function(name = "expect")]
    fn expect(cx: &mut ModuleCx<'_, Self>, actual: JsValue) -> JsResult<Instance<Matcher>> {
        let instance = cx.alloc(Matcher)?;
        instance.set_actual(cx, actual)?;
        Ok(instance)
    }
}

#[host_class(tag = "Matcher", methods)]
struct Matcher {
    #[js_slot]
    actual: JsValue,
}

#[methods(Matcher)]
impl Matcher {
    #[method(name = "toEqual")]
    fn to_equal(cx: &mut CallCx<'_>, this: This<Matcher>, expected: JsValue) -> JsResult<()> {
        let actual = this.actual(cx)?;
        check_own_data_equal(cx, actual, expected)
    }
}
```

一个 VM 中的 `Matcher` 实例共享原型和方法：`a.toEqual === b.toEqual`。方法检查接收者是否具有 `Matcher` 的内部槽；裸调或用仅伪造了原型的对象作 `this`，抛出可捕获的 `TypeError`。跨 `await` 保存方法时用 `a.toEqual.bind(a)`，或保留 `a` 并作成员调用。注册类型不隐式导出 JS 构造器：上述公开签名足以生成 `export function expect(actual: unknown): Matcher` 和带有 `toEqual(expected: unknown): void` 的 `interface Matcher`，不生成可 `new` 的值，也不公开内部槽。

## JS 值槽与非 JS 数据

- `JsValue` 句柄仅用于当前 VM 的本次同步调用，不可藏在 Rust 数据或注册闭包中供下次调用。要长期持有任意 JS 值或另一个绑定实例，写入声明的 VM 托管槽。槽不是可枚举或可伪造的 JS 属性，而是对象图的强边；与普通引用一起追踪，保留身份、别名和环，不把所有实例提升为根。
- `expect(x)` 把 `x` 本身放进槽，不投影或复制对象图。`toBe` 比较原值（`SameValue`）；`toEqual` 在调用方法时读取当时的自有可枚举数据，不执行 getter 或 Proxy trap。因此在 `expect(obj)` 之后修改 `obj.x`，后续比较能看到修改。断言失败以脚本可捕获的异常返回。
- 需要 Rust 状态的实例可附带有类型、有版本的 payload。模块、类和 operation 共用接收当前 `&Services` 的 `Encode` / `Decode`；可派生字段级 codec 或手写业务校验。**服务提供能力，快照提供状态**：例如解码时用当前 scope 的平台工厂和快照里的属性创建节点，而不是去旧宿主按节点 ID 取回对象。payload 不得暗藏指向 VM 对象的 ID；这类边一律走槽，无需给绑定作者另设追踪协议。
- 会影响后续调用的模块级可变状态属于 VM 托管的模块记录，作为持久根保存；JS 引用和非 JS 数据分别遵循上述槽和 codec 规则。静态模块描述只包含声明及实现入口，不持有某个 VM 的实例状态。

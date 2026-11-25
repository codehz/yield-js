use std::rc::Rc;

use yield_compiler::{
    CompileError, Compiler, GraphTransform, MemoryResolver, ModuleGraph, ParsedModule, Project,
    SourceTransform, TransformError,
};
use yield_vm::{Bindings, Build, Host, JsValue, ValueRef, Vm};

fn compile(
    source: &str,
    runtime: Option<(&str, &str)>,
    compiler: &Compiler,
) -> Result<Build, CompileError> {
    let resolver = MemoryResolver::new([("main.jsx", source)].into_iter().chain(runtime)).unwrap();
    compiler.compile(Project::new("main.jsx", resolver))
}

struct EmptyHost(Rc<Bindings>);

impl EmptyHost {
    fn new() -> Self {
        let bindings = yield_vm::__macro_support::link([], Default::default(), Default::default());
        Self(Rc::new(bindings.unwrap()))
    }
}

impl Host for EmptyHost {
    fn bindings(&self) -> Rc<Bindings> {
        Rc::clone(&self.0)
    }

    fn report_error(&mut self, error: JsValue) {
        panic!("unexpected guest error: {error:?}");
    }
}

#[test]
fn jsx_uses_hygienic_runtime_helpers_and_evaluates_inputs_in_source_order() {
    let source = r#"
export function run($jsx0 = 'local') {
    const $jsx1 = 'binding';
    const $jsx2 = 'another binding';
    let order = '';
    const mark = value => { order += value; return value; };
    const C = { get Tag() { mark('T'); return 'component'; } };
    const spread = { get x() { return mark('S'); }, key: 'spread key' };
    const node = <C.Tag x={mark('A')} key={mark('K')} {...spread} enabled
        key={mark('F')} label="&lt; &amp;" __proto__="data">
        hello &amp;
        world
        {/* omitted */}
        {mark('C')}
        <><i />{$jsx0 + $jsx1 + $jsx2}</>
    </C.Tag>;
    const spreadOnly = <div {...spread} />;
    return order === 'TAKSFCS' && node.helper === 'jsxs' && node.tag === 'component'
        && node.key === 'F' && !('key' in node.props) && node.props.enabled
        && node.props.x === 'S' && node.props.label === '< &' && node.props.__proto__ === 'data'
        && node.props.children[0] === 'hello & world' && node.props.children[1] === 'C'
        && node.props.children[2].tag === 'fragment'
        && node.props.children[2].props.children[0].tag === 'i'
        && node.props.children[2].props.children[0].helper === 'jsx'
        && node.props.children[2].props.children[1] === 'localbindinganother binding'
        && spreadOnly.props.key === 'spread key' && spreadOnly.tag === 'div';
}
"#;
    let runtime = r#"
export const Fragment = 'fragment';
export function jsx(tag, props, key) { return { helper: 'jsx', tag, props, key }; }
export function jsxs(tag, props, key) { return { helper: 'jsxs', tag, props, key }; }
"#;
    let compiler = Compiler {
        jsx_runtime: "@example/view".into(),
        ..Compiler::default()
    };
    let build = compile(
        source,
        Some(("@example/view/jsx-runtime", runtime)),
        &compiler,
    )
    .unwrap();
    let mut host = EmptyHost::new();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert!(matches!(
        vm.start(&mut host, "run", vec![]).unwrap().view(),
        ValueRef::Bool(true)
    ));
}

#[test]
fn jsx_links_only_used_helpers_and_does_not_import_for_plain_modules() {
    let compiler = Compiler::default();
    compile("export const n = 1;", None, &compiler).unwrap();
    compile(
        "export default <item>{/* no child */}</item>;",
        Some(("react/jsx-runtime", "export function jsx() {}")),
        &compiler,
    )
    .unwrap();
}

#[test]
fn jsx_diagnostics_distinguish_original_and_generated_spans() {
    let compiler = Compiler::default();
    for runtime in [None, Some(("react/jsx-runtime", "export const wrong = 0;"))] {
        let error = compile("\nexport default <div />;", runtime, &compiler).unwrap_err();
        assert_eq!(error.module(), "main.jsx");
        assert_eq!(error.stage(), Some("JSX transform"));
        assert_eq!((error.line(), error.column()), (0, 0));
        assert!(error.message().contains(if runtime.is_some() {
            "no export"
        } else {
            "react/jsx-runtime"
        }));
    }

    let error = compile("\nexport default <ns:tag />;", None, &compiler).unwrap_err();
    assert_eq!(error.module(), "main.jsx");
    assert_eq!(error.stage(), Some("JSX transform"));
    assert_eq!((error.line(), error.column()), (2, 17));

    let runtime = Some(("react/jsx-runtime", "export function jsx() {}"));
    let error = compile(
        "\nexport default <div>{missingName}</div>;",
        runtime,
        &compiler,
    )
    .unwrap_err();
    assert_eq!(error.module(), "main.jsx");
    assert_eq!((error.line(), error.column()), (2, 22));
    assert!(error.message().contains("missingName"));
}

#[test]
fn jsx_replacement_runs_before_discovery_and_again_on_relink() {
    struct Source;
    impl SourceTransform for Source {
        fn transform(&mut self, module: &mut ParsedModule<'_>) -> Result<(), TransformError> {
            if module.id() == "main.jsx" {
                module.replace_source("export default <before />;")?;
            }
            Ok(())
        }
    }

    struct Graph;
    impl GraphTransform for Graph {
        fn transform(&mut self, graph: &mut ModuleGraph<'_>) -> Result<(), TransformError> {
            let entry = graph.entry().clone();
            let deps = graph.dependencies(&entry).unwrap();
            assert_eq!(deps.len(), 1);
            assert_eq!(deps[0].specifier, "react/jsx-runtime");
            graph
                .module_mut(&entry)
                .unwrap()
                .replace_source("export default <after />;")?;
            Ok(())
        }
    }

    let resolver = MemoryResolver::new([
        ("main.jsx", "export default 0;"),
        (
            "react/jsx-runtime",
            "export function jsx(tag) { return tag === 'after'; }",
        ),
    ])
    .unwrap();
    let build = Compiler::default()
        .compile(
            Project::new("main.jsx", resolver)
                .with_source_transform(Source)
                .with_graph_transform(Graph),
        )
        .unwrap();
    let mut host = EmptyHost::new();
    let vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert!(matches!(
        vm.get_export("default").unwrap().view(),
        ValueRef::Bool(true)
    ));
}

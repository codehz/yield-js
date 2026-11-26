use oxc::ast::ast::{ImportOrExportKind, Statement, StringLiteral};
use oxc::ast::builder::AstBuilder;
use oxc::span::{GetSpan, SPAN};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use yield_compiler::{
    BuildTransform, CompileError, Compiler, DependencyKind, GraphTransform, LoadedModule,
    MemoryResolver, ModuleGraph, ModuleId, ModuleResolver, NodeResolver, ParsedModule, Project,
    SourceTransform, TransformError,
};
use yield_vm::bytecode::{Build, BuildResource, CallArgs, Instr, ModuleTarget, SourceModule};

fn compile(source: &str, options: &Compiler) -> Result<Build, CompileError> {
    let resolver = MemoryResolver::new([("main.js", source)]).unwrap();
    options.compile(Project::new("main.js", resolver))
}

fn unexpected_load_error(message: String) -> CompileError {
    panic!("unexpected load error: {message}")
}

fn entry_module(build: &Build) -> &SourceModule {
    &build.modules[build.entry as usize]
}

fn compile_with_preload(
    source: &str,
    preload: &str,
    options: &Compiler,
) -> Result<Build, CompileError> {
    let resolver = MemoryResolver::new([("main.js", source), ("preload.js", preload)]).unwrap();
    options.compile(Project::new("main.js", resolver).with_preload("preload.js"))
}

struct Rewrite(&'static str, &'static str);

impl SourceTransform for Rewrite {
    fn transform(&mut self, module: &mut ParsedModule<'_>) -> Result<(), TransformError> {
        if module.id() == "main.js" {
            assert_eq!(module.source(), self.0);
            module.replace_source(self.1)?;
        }
        Ok(())
    }
}

impl GraphTransform for Rewrite {
    fn transform(&mut self, graph: &mut ModuleGraph<'_>) -> Result<(), TransformError> {
        let entry = graph.entry().clone();
        SourceTransform::transform(self, graph.module_mut(&entry).unwrap())
    }
}

#[test]
fn pipeline_runs_each_stage_in_registration_order_and_relinks_exports() {
    struct BuildStage(&'static str, &'static str);
    impl BuildTransform for BuildStage {
        fn transform(&mut self, build: &mut Build) -> Result<(), TransformError> {
            assert_eq!(build.build_id, build.compute_id(), "stage input finalized");
            let entry = &mut build.modules[build.entry as usize];
            let export = entry.exports.remove(self.0).expect("prior stage export");
            entry.exports.insert(self.1.into(), export);
            Ok(())
        }
    }
    let first = "export const first = 1;";
    let second = "export const second = 2;";
    let third = "export { value as third } from './dep.js';";
    let fourth = "export { value as fourth } from './dep.js';";
    let fifth = "export { value as fifth } from './dep.js';";
    let resolver =
        MemoryResolver::new([("main.js", first), ("dep.js", "export const value = 3;")]).unwrap();
    let build = Compiler::default()
        .compile(
            Project::new("main.js", resolver)
                .with_graph_transform(Rewrite(third, fourth))
                .with_build_transform(BuildStage("fifth", "sixth"))
                .with_source_transform(Rewrite(first, second))
                .with_source_transform(Rewrite(second, third))
                .with_graph_transform(Rewrite(fourth, fifth))
                .with_build_transform(BuildStage("sixth", "final")),
        )
        .unwrap();
    let entry = entry_module(&build);
    assert_eq!(
        entry.exports.keys().map(String::as_str).collect::<Vec<_>>(),
        ["final"]
    );
    assert!(
        matches!(entry.exports["final"].target, ModuleTarget::Binding { module, .. }
        if build.modules[module as usize].id == "dep.js")
    );
    assert_eq!(build.build_id, build.compute_id());
    build.validate().unwrap();
}

#[test]
fn build_transforms_receive_only_resources_from_the_final_graph() {
    struct CheckResources;
    impl BuildTransform for CheckResources {
        fn transform(&mut self, build: &mut Build) -> Result<(), TransformError> {
            let resources: Vec<_> = build
                .resources
                .iter()
                .map(|resource| (resource.uri.as_str(), &*resource.bytes))
                .collect();
            assert_eq!(
                resources,
                [("asset:keep.txt", &b"keep"[..]), ("asset:extra", &b""[..])]
            );
            assert_eq!(build.build_id, build.compute_id());
            assert!(build.modules.iter().all(|module| module.id != "drop.txt"));
            Ok(())
        }
    }

    let root = TempDir::new();
    let source = "import './drop.txt'; import './keep.txt';";
    fixture(&root, "main.js", source);
    fixture(&root, "drop.txt", "drop");
    fixture(&root, "keep.txt", "keep");
    let resolver = NodeResolver::new(&root.0)
        .unwrap()
        .with_loader("txt", |id, text| {
            let mut loaded = LoadedModule::source("");
            loaded.resources.push(BuildResource {
                uri: format!("asset:{id}"),
                bytes: text.into_bytes().into(),
            });
            if id == "keep.txt" {
                loaded.resources.push(BuildResource {
                    uri: "asset:extra".into(),
                    bytes: [].into(),
                });
            }
            Ok(loaded)
        });
    Compiler::default()
        .compile(
            Project::new("main.js", resolver)
                .with_graph_transform(Rewrite(source, "import './keep.txt';"))
                .with_build_transform(CheckResources),
        )
        .unwrap();
}

#[test]
fn duplicate_module_resource_uris_fail_without_a_build_transform() {
    let root = TempDir::new();
    fixture(&root, "main.js", "import './one.txt'; import './two.txt';");
    fixture(&root, "one.txt", "one");
    fixture(&root, "two.txt", "two");
    let resolver = NodeResolver::new(&root.0)
        .unwrap()
        .with_loader("txt", |_, text| {
            let mut loaded = LoadedModule::source("");
            loaded.resources.push(BuildResource {
                uri: "asset:shared".into(),
                bytes: text.into_bytes().into(),
            });
            Ok(loaded)
        });
    let error = Compiler::default()
        .compile(Project::new("main.js", resolver))
        .unwrap_err();
    assert_eq!(error.stage(), Some("module resources"));
    assert_eq!(
        error.message(),
        "duplicate build resource URI \"asset:shared\""
    );
}

#[test]
fn ast_transforms_discover_imports_and_relink_exports_without_reparsing() {
    struct InsertImport;
    impl SourceTransform for InsertImport {
        fn transform(&mut self, module: &mut ParsedModule<'_>) -> Result<(), TransformError> {
            if module.id() == "main.js" {
                let ast = AstBuilder::new(module.allocator());
                let source = StringLiteral::new(SPAN, "./new.js", None, &ast);
                module
                    .program_mut()
                    .body
                    .push(Statement::new_import_declaration(
                        SPAN,
                        None,
                        source,
                        None,
                        None,
                        ImportOrExportKind::Value,
                        &ast,
                    ));
            }
            Ok(())
        }
    }

    struct Retarget;
    impl GraphTransform for Retarget {
        fn transform(&mut self, graph: &mut ModuleGraph<'_>) -> Result<(), TransformError> {
            let entry = graph.entry().clone();
            let module = graph.module_mut(&entry).unwrap();
            let ast = AstBuilder::new(module.allocator());
            let Statement::ExportFromDeclaration(export) = &mut module.program_mut().body[0] else {
                panic!("expected a re-export");
            };
            export.source = StringLiteral::new(export.source.span, "./new.js", None, &ast);
            Ok(())
        }
    }

    let resolver = MemoryResolver::new([
        ("main.js", "export { value } from './old.js';"),
        ("old.js", "export const value = 1;"),
        ("new.js", "export const value = 2;"),
    ])
    .unwrap();
    let build = Compiler::default()
        .compile(
            Project::new("main.js", resolver)
                .with_source_transform(InsertImport)
                .with_graph_transform(Retarget),
        )
        .unwrap();
    assert!(
        matches!(entry_module(&build).exports["value"].target, ModuleTarget::Binding { module, .. }
        if build.modules[module as usize].id == "new.js")
    );
    assert!(build.modules.iter().all(|module| module.id != "old.js"));
}

#[test]
fn pipeline_attributes_transform_failures_and_stops_before_later_stages() {
    struct FailSource;
    impl SourceTransform for FailSource {
        fn transform(&mut self, module: &mut ParsedModule<'_>) -> Result<(), TransformError> {
            module.replace_source("\nexport default 2;")?;
            let span = module.program().body[0].span();
            Err(TransformError::new("rejected transformed source").at(span))
        }
    }
    struct InvalidBuild;
    impl BuildTransform for InvalidBuild {
        fn transform(&mut self, build: &mut Build) -> Result<(), TransformError> {
            build.entry = u32::MAX;
            Ok(())
        }
    }
    struct UnreachableBuild;
    impl BuildTransform for UnreachableBuild {
        fn transform(&mut self, _: &mut Build) -> Result<(), TransformError> {
            panic!("a preceding stage should have failed")
        }
    }
    let resolver = MemoryResolver::new([("main.js", "export default 1;")]).unwrap();
    let compiler = Compiler::default();
    let error = compiler
        .compile(
            Project::new("main.js", &resolver)
                .with_source_transform(FailSource)
                .with_build_transform(UnreachableBuild),
        )
        .unwrap_err();
    assert_eq!(error.stage(), Some("source transform 1"));
    assert_eq!(error.module(), "main.js");
    assert_eq!((error.line(), error.column()), (2, 1));
    assert_eq!(error.message(), "rejected transformed source");

    let error = compiler
        .compile(
            Project::new("main.js", &resolver)
                .with_build_transform(InvalidBuild)
                .with_build_transform(UnreachableBuild),
        )
        .unwrap_err();
    assert_eq!(error.stage(), Some("build transform 1"));
    assert!(
        error.message().contains("invalid source module index"),
        "{error}"
    );
}

#[test]
fn compiles_overflowing_number_literals() {
    let build = compile(
        "export function run() { return [1e999, -1e999]; }",
        &Compiler::default(),
    )
    .unwrap();
    assert!(build.constants.iter().any(|constant| matches!(
        constant,
        yield_vm::bytecode::Constant::Number(value) if *value == f64::INFINITY
    )));
    build.validate().unwrap();
}

#[test]
fn compiles_preload_and_main_in_separate_module_scopes() {
    let options = Compiler::default();
    let build = compile_with_preload(
        "export function shared() { return 'same'; }",
        "export function shared() { return 'same'; }",
        &options,
    )
    .unwrap();
    let entry = &build.modules[build.entry as usize];
    let preload = &build.modules[build.preload.unwrap() as usize];
    assert_eq!(build.function(entry.evaluation).unwrap().name, "\0script");
    assert_eq!(
        build.function(preload.evaluation).unwrap().name,
        "\0preload"
    );
    assert_ne!(
        entry.exports["shared"].target,
        preload.exports["shared"].target
    );
    assert_eq!(
        build
            .constants
            .iter()
            .filter(|constant| matches!(
                constant, yield_vm::bytecode::Constant::String(value) if value == "same"
            ))
            .count(),
        1
    );

    let error = compile_with_preload("", "\nmissing;", &options).unwrap_err();
    assert_eq!(error.module(), "preload.js");
    assert_eq!(error.line(), 2);

    let error = compile_with_preload("\nmissing;", "", &options).unwrap_err();
    assert_eq!(error.module(), "main.js");
    assert_eq!(error.line(), 2);
}

#[test]
fn validates_module_initializers_and_export_bindings() {
    let build = compile_with_preload(
        "export let main = 0; export function run() {}",
        "export let n = 1;",
        &Compiler::default(),
    )
    .unwrap();
    let entry = build.entry as usize;
    let preload = build.preload.unwrap() as usize;
    let mut changed = build.clone();
    changed.modules[preload].evaluation = changed.modules[entry].evaluation;
    assert!(changed.validate().unwrap_err().contains("build_id"));
    changed.finalize();
    assert!(changed.validate().unwrap_err().contains("is not distinct"));

    let mut changed = build.clone();
    changed.modules[preload].evaluation = u32::MAX;
    changed.finalize();
    assert!(
        changed
            .validate()
            .unwrap_err()
            .contains("missing initializer")
    );

    let mut changed = build.clone();
    let ModuleTarget::Binding { binding, .. } = changed.modules[entry].exports["main"].target
    else {
        panic!("expected a local export binding");
    };
    changed.modules[preload]
        .exports
        .get_mut("n")
        .unwrap()
        .target = ModuleTarget::Binding {
        module: preload as u32,
        binding,
    };
    changed.finalize();
    assert!(
        changed
            .validate()
            .unwrap_err()
            .contains("is not declared in module")
    );

    let mut changed = build;
    let initializer = changed.modules[preload].evaluation;
    let declaration = changed.modules[entry].declaration;
    let main = changed
        .functions
        .iter_mut()
        .find(|f| f.func_id == declaration)
        .unwrap();
    let closure = main
        .code
        .iter_mut()
        .find_map(|op| match op {
            Instr::Closure { function, .. } => Some(function),
            _ => None,
        })
        .unwrap();
    *closure = initializer;
    changed.finalize();
    assert!(
        changed
            .validate()
            .unwrap_err()
            .contains("references reserved function")
    );
}

#[test]
fn rejects_module_getters_reading_the_wrong_binding() {
    let mut build =
        compile_with_preload("", "export let x = 1, y = 2;", &Compiler::default()).unwrap();
    let exports = &mut build.modules[build.preload.unwrap() as usize].exports;
    exports.get_mut("x").unwrap().getter = exports["y"].getter;
    assert!(build.validate().unwrap_err().contains("build_id"));
    build.finalize();
    assert!(build.validate().unwrap_err().contains("invalid getter"));
}

#[test]
fn builtin_indices_preserve_bytecode_abi() {
    let original = [
        "Promise",
        "queueMicrotask",
        "Error",
        "TypeError",
        "Symbol",
        "Reflect",
        "Object",
        "NaN",
        "Infinity",
        "Array",
        "String",
        "RangeError",
        "ReferenceError",
        "SyntaxError",
        "EvalError",
        "URIError",
        "AggregateError",
        "Number",
        "Math",
        "JSON",
        "Map",
        "Set",
        "WeakMap",
        "Proxy",
        "Boolean",
        "globalThis",
        "RegExp",
        "WeakSet",
    ];
    assert!(yield_vm::bytecode::BUILTINS.starts_with(&original));
}

fn manifest(modules: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
    modules
        .iter()
        .map(|(module, exports)| {
            (
                (*module).to_owned(),
                exports.iter().map(|export| (*export).to_owned()).collect(),
            )
        })
        .collect()
}

const RUN: &str = r#"
async function run(input) {
  inspect(input);
  const base = seed();
  const x = await wait("a");
  const sum = base + x;
  const y = await wait("b");
  return sum + y;
}
"#;

fn compile_run(source: &str) -> yield_vm::bytecode::Function {
    let source = format!("import {{ inspect, seed, wait }} from \"test\";\n{source}");
    let build = compile(
        &source,
        &Compiler {
            imports: manifest(&[("test", &["inspect", "seed", "wait"])]),
            ..Compiler::default()
        },
    )
    .expect("source compiles");
    build
        .functions
        .into_iter()
        .find(|function| function.name == "run")
        .expect("missing run")
}

#[test]
fn function_length_stops_at_top_level_default_or_rest() {
    let build = compile(
        r#"
function run({ x = 1 }, [y], z = 2, last, ...tail) {
  return [(a, ...rest) => a, (...rest) => rest, (a = 1, b) => b];
}
"#,
        &Compiler::default(),
    )
    .expect("source compiles");
    let run = build.function_named("run").expect("missing run");
    assert_eq!(run.length, 2);
    assert_eq!(run.params.len(), 4);
    assert!(run.rest_param.is_some());
    let lengths: Vec<_> = build
        .functions
        .iter()
        .filter(|function| function.is_arrow)
        .map(|function| function.length)
        .collect();
    assert_eq!(lengths, [1, 0, 0]);
}

#[test]
fn object_methods_use_statically_known_key_hints() {
    let build = compile(
        r#"
function run(key) {
  return {
    plain(a, b = 1) {},
    ["literal"](a, ...rest) {},
    get value() {},
    set value(v) {},
    [key]() {},
  };
}
"#,
        &Compiler::default(),
    )
    .expect("source compiles");
    let methods: Vec<_> = build
        .functions
        .iter()
        .filter(|function| function.is_method)
        .map(|function| {
            let (hint, _) = function.name.rsplit_once('@').expect("missing function ID");
            (hint, function.length)
        })
        .collect();
    assert_eq!(
        methods,
        [
            ("plain", 1),
            ("literal", 1),
            ("get value", 0),
            ("set value", 1),
            ("method", 0),
        ]
    );
}

#[test]
fn live_locals_add_one_saved_slot_beyond_empty_awaits() {
    let subject = compile_run(RUN);
    let baseline = compile_run(
        r#"
async function run() {
  await wait("a");
  await wait("b");
}
"#,
    );
    assert!(subject.is_async);
    assert!(baseline.is_async);
    for function in [&subject, &baseline] {
        let await_count = function
            .code
            .iter()
            .filter(|instruction| matches!(instruction, Instr::Await { .. }))
            .count();
        assert_eq!(function.resumes.len(), await_count);
    }
    let deltas: Vec<_> = subject
        .resumes
        .iter()
        .zip(&baseline.resumes)
        .map(|(subject, baseline)| {
            subject
                .saved_slots
                .len()
                .checked_sub(baseline.saved_slots.len())
                .expect("subject saved fewer slots than baseline")
        })
        .collect();
    assert_eq!(deltas, vec![1, 1]);
}

#[test]
fn compiles_tagged_templates_as_call_sites() {
    let build = compile(
        "export function run(tag) { return tag`a${1}\\n`; }",
        &Compiler::default(),
    )
    .unwrap();
    assert_eq!(build.templates.len(), 1);
    let site = &build.templates[0];
    assert_eq!(site.raw.len(), 2);
    assert_eq!(site.raw[0].to_utf8().unwrap(), "a");
    assert_eq!(site.raw[1].to_utf8().unwrap(), "\\n");
    assert_eq!(site.cooked.len(), 2);
    assert_eq!(site.cooked[0].as_ref().unwrap().to_utf8().unwrap(), "a");
    assert_eq!(site.cooked[1].as_ref().unwrap().to_utf8().unwrap(), "\n");
    let run = build.function_named("run").unwrap();
    assert!(
        run.code
            .iter()
            .any(|instruction| matches!(instruction, Instr::GetTemplate { template: 0, .. }))
    );
    build.validate().unwrap();
}

#[test]
fn compiles_invalid_tagged_escapes_without_cooked_text() {
    let build = compile(
        "export function run(tag) { return tag`\\unicode`; }",
        &Compiler::default(),
    )
    .unwrap();
    assert_eq!(build.templates[0].cooked, vec![None]);
    assert_eq!(build.templates[0].raw[0].to_utf8().unwrap(), "\\unicode");
    build.validate().unwrap();
}

#[test]
fn rejects_unsupported_syntax_with_location() {
    let error = compile("function run() { debugger; }", &Compiler::default())
        .expect_err("debugger statements are outside the PoC subset");
    assert!(error.message().contains("unsupported syntax"));
    assert_eq!(error.line(), 1);
    assert!(error.column() > 1);
}

#[test]
fn rejects_invalid_private_names() {
    for (source, message) in [
        (
            "function f(o) { return o.#x; }",
            "undeclared private name #x",
        ),
        (
            "class C { m(o) { return #x in o; } }",
            "undeclared private name #x",
        ),
        ("class C { #x; #x() {} }", "duplicate private name #x"),
        (
            "class C { get #x() {} static set #x(v) {} }",
            "duplicate private name #x",
        ),
        (
            "class C { get #x() {} set #x(v) {} get #x() {} }",
            "duplicate private name #x",
        ),
        ("class C { #x; m() { delete (this.#x); } }", "private"),
        ("class C { #x; m() { delete this?.#x; } }", "private"),
        ("class C { #x; m() { return super.#x; } }", "private"),
        ("class C { #constructor; }", "constructor"),
        (
            "class C extends (#x in {} ? Object : Object) { #x; }",
            "undeclared private name #x",
        ),
    ] {
        let error = compile(source, &Compiler::default()).expect_err(source);
        assert!(
            error.message().to_lowercase().contains(message),
            "{source}: {error}"
        );
        assert_eq!(error.line(), 1, "{source}: {error}");
        assert!(error.column() > 1, "{source}: {error}");
    }
}

#[test]
fn non_captured_loop_head_bindings_skip_iteration_environments() {
    let build = compile(
        r#"
function run(n) {
  let total = 0;
  for (let i = 0; i < n; i += 1) {
    let doubled = i + i;
    total += doubled;
  }
  for (const value of [1, 2, 3]) total += value;
  for (const key in { a: 1 }) total += key.length;
  return total;
}
"#,
        &Compiler::default(),
    )
    .expect("source compiles");
    let run = build.function_named("run").expect("missing run");
    assert!(
        !run.code.iter().any(|instruction| matches!(
            instruction,
            Instr::SaveEnv { .. } | Instr::NewIterationEnv { .. } | Instr::RestoreEnv { .. }
        )),
        "non-captured loop heads must not allocate iteration environments"
    );
    build.validate().unwrap();
}

#[test]
fn captured_loop_head_bindings_keep_iteration_environments() {
    let build = compile(
        r#"
function run() {
  const out = [];
  for (let i = 0; i < 2; i += 1) out.push(() => i);
  for (const value of [1, 2]) out.push(() => value);
  return out;
}
"#,
        &Compiler::default(),
    )
    .expect("source compiles");
    let run = build.function_named("run").expect("missing run");
    assert!(
        run.code
            .iter()
            .any(|instruction| matches!(instruction, Instr::SaveEnv { .. }))
    );
    assert!(run.code.iter().any(|instruction| matches!(
        instruction,
        Instr::NewIterationEnv { bindings, .. } if !bindings.is_empty()
    )));
    assert!(
        run.code
            .iter()
            .any(|instruction| matches!(instruction, Instr::RestoreEnv { .. }))
    );
    build.validate().unwrap();
}

#[test]
fn mixed_loop_head_escapes_copy_only_the_captured_binding() {
    let build = compile(
        r#"
function run() {
  const out = [];
  for (let i = 0, j = 0; i < 2; i += 1, j += 10) out.push(() => i);
  return out;
}
"#,
        &Compiler::default(),
    )
    .expect("source compiles");
    let run = build.function_named("run").expect("missing run");
    let mut enters = 0;
    for instruction in &run.code {
        if let Instr::NewIterationEnv { bindings, .. } = instruction {
            enters += 1;
            assert_eq!(
                bindings.len(),
                1,
                "only the captured loop binding is copied"
            );
        }
    }
    assert_eq!(enters, 2, "a C-style for enters before test and update");
    build.validate().unwrap();
}

#[test]
fn promoted_loop_head_keeps_environment_for_captured_body_bindings() {
    let build = compile(
        r#"
function run() {
  const out = [];
  for (let i = 0; i < 2; i += 1) {
    let x = i;
    out.push(() => x);
  }
  return out;
}
"#,
        &Compiler::default(),
    )
    .expect("source compiles");
    let run = build.function_named("run").expect("missing run");
    let enters: Vec<_> = run
        .code
        .iter()
        .filter_map(|instruction| match instruction {
            Instr::NewIterationEnv { bindings, .. } => Some(bindings),
            _ => None,
        })
        .collect();
    assert_eq!(
        enters.len(),
        2,
        "a C-style for enters before test and update"
    );
    assert!(
        enters.iter().all(|bindings| bindings.is_empty()),
        "promoted loop-head bindings are not copied"
    );
    assert!(
        run.code
            .iter()
            .any(|instruction| matches!(instruction, Instr::SaveEnv { .. }))
    );
    assert!(
        run.code
            .iter()
            .any(|instruction| matches!(instruction, Instr::RestoreEnv { .. }))
    );
    build.validate().unwrap();
}

#[test]
fn promoted_loop_head_across_await_uses_a_persistent_slot() {
    let build = compile(
        r#"
async function run() {
  let total = 0;
  for (let i = 0; i < 2; i += 1) {
    total += await i;
  }
  return total;
}
"#,
        &Compiler::default(),
    )
    .expect("source compiles");
    let run = build.function_named("run").expect("missing run");
    assert!(!run.code.iter().any(|instruction| matches!(
        instruction,
        Instr::SaveEnv { .. } | Instr::NewIterationEnv { .. } | Instr::RestoreEnv { .. }
    )));
    assert!(
        run.resumes
            .iter()
            .any(|resume| !resume.saved_slots.is_empty())
    );
    build.validate().unwrap();
}

#[test]
fn for_await_lowers_the_async_protocol_across_awaits() {
    let function = compile_run(
        r#"
async function run(source) {
  let total = 0;
  for await (const value of source) total += value;
  return total;
}
"#,
    );
    let iterator = function
        .code
        .iter()
        .find_map(|instruction| match instruction {
            Instr::GetIterator { iterator, .. } => Some(*iterator),
            _ => None,
        })
        .expect("for-await emits the synchronous fallback");
    assert_eq!(iterator.kind, yield_vm::bytecode::SlotKind::Persistent);
    assert!(
        function
            .resumes
            .iter()
            .any(|resume| resume.saved_slots.contains(&iterator.index))
    );
    assert!(
        function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instr::Await { .. }))
    );
}

#[test]
fn for_in_keeps_enumerator_live_across_await_without_iterator_close() {
    let function = compile_run(
        r#"
async function run(source, target) {
  outer: inner: for (target[seed()] in source) {
    if (await wait(target.key)) continue outer;
    break inner;
  }
}
"#,
    );
    let iterator = function
        .code
        .iter()
        .find_map(|instruction| match instruction {
            Instr::GetForInIterator { dst, .. } => Some(*dst),
            _ => None,
        })
        .expect("missing for-in setup");
    assert_eq!(iterator.kind, yield_vm::bytecode::SlotKind::Persistent);
    assert!(function.code.iter().any(|instruction| matches!(
        instruction,
        Instr::ForInNext { iterator: step, .. } if *step == iterator
    )));
    assert!(function.resumes[0].saved_slots.contains(&iterator.index));
    assert!(!function.code.iter().any(|instruction| matches!(
        instruction,
        Instr::GetIterator { .. } | Instr::IteratorNext { .. } | Instr::IteratorClose { .. }
    )));
}

#[test]
fn for_in_collects_vars_but_keeps_lexical_bindings_in_the_header_scope() {
    compile_run(
        r#"
function run(source) {
  inspect(key, fromBody);
  for (var key in source) { var fromBody = key; }
  for (key in source) {}
  for (let source in {}) { inspect(source); }
  for (const key in source) { inspect(key); }
  return key;
}
"#,
    );
    compile(
        "export { key }; for (var key in {}) {}",
        &Compiler::default(),
    )
    .expect("script scan collects a for-in var");
    for (statement, diagnostic) in [
        ("for (let key in key) {}", "before initialization"),
        ("for (const key in {}) {} return key;", "unknown binding"),
        (
            "for (let key in {}) { var key; }",
            "duplicate lexical binding",
        ),
        ("for (missing in {}) {}", "assignment to unknown binding"),
        (
            "for (missing in {}) {} let missing;",
            "before initialization",
        ),
    ] {
        let source = format!("function run() {{\n  {statement}\n}}");
        let error = compile(&source, &Compiler::default()).expect_err(statement);
        assert!(error.message().contains(diagnostic), "{statement}: {error}");
        assert_eq!(error.line(), 2, "{statement}: {error}");
    }
}

#[test]
fn rejects_delete_of_identifier_in_strict_modules() {
    for operand in ["x", "((x))"] {
        let source = format!("function run(x) {{\n  return delete {operand};\n}}");
        let error = compile(&source, &Compiler::default()).expect_err(&source);
        assert_eq!(error.line(), 2);
        assert!(error.message().contains("delete"), "{error}");
    }
}

#[test]
fn rejects_non_lvalue_update_targets() {
    for expression in ["++1", "(1 + 2)--"] {
        let source = format!("function run() {{\n  return {expression};\n}}");
        let error = compile(&source, &Compiler::default()).expect_err(&source);
        assert_eq!(error.line(), 2, "{source}: {error}");
    }
}

#[test]
fn rejects_compound_destructuring_assignment() {
    for (target, operator) in [
        ("[a]", "+="),
        ("[a]", "&&="),
        ("[a]", "??="),
        ("{a}", "+="),
        ("{a}", "&&="),
        ("{a}", "??="),
    ] {
        let source = format!("function run(a,b) {{\n  ({target} {operator} b);\n}}");
        let error = compile(&source, &Compiler::default()).expect_err(&source);
        assert_eq!(error.line(), 2, "{source}: {error}");
        assert!(
            error
                .message()
                .contains("Invalid left-hand side in assignment"),
            "{source}: {error}"
        );
    }
}

#[test]
fn rejects_unsupported_optional_chain_syntax_with_location() {
    for (source, message, column) in [
        (
            "function run(){ return null?.[missing]; }",
            "unknown binding \"missing\"",
            31,
        ),
        (
            "function run(){ return missing?.(); }",
            "unknown callee \"missing\"",
            24,
        ),
    ] {
        let error = compile(source, &Compiler::default()).expect_err(source);
        assert!(error.message().contains(message), "{source}: {error}");
        assert_eq!(error.line(), if source.contains('\n') { 2 } else { 1 });
        assert_eq!(error.column(), column, "{source}: {error}");
    }
}

#[test]
fn rejects_invalid_label_uses_with_location() {
    let cases = [
        "function run() { break missing; }",
        "function run() { continue missing; }",
        "function run() { continue notaloop; notaloop: { } }",
        "function run() { same: same: while (true) { break same; } }",
        "function run() { outer: while (true) { function inner() { break outer; } } }",
    ];
    for source in cases {
        let error =
            compile(source, &Compiler::default()).expect_err("label misuse must be rejected");
        assert!(error.line() >= 1, "{source}: {error}");
    }
}

#[test]
fn compiles_object_spread() {
    let build = compile(
        "function run(other) { return { before: 1, ...other, after: 2 }; }",
        &Compiler::default(),
    )
    .expect("object spread compiles");
    let function = build
        .functions
        .iter()
        .find(|function| function.name == "run")
        .unwrap();
    assert!(
        function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instr::CopyDataProperties { .. }))
    );
}

#[test]
fn rejects_malformed_property_definition_and_method_metadata() {
    let build = compile(
        "function run() { return { m() {} }; }",
        &Compiler::default(),
    )
    .expect("method");
    for (operand, message) in ["object", "key", "src"].into_iter().enumerate() {
        let mut corrupt = build.clone();
        let (object, key, src) = corrupt
            .functions
            .iter_mut()
            .find(|f| f.name == "run")
            .unwrap()
            .code
            .iter_mut()
            .find_map(|instr| {
                let Instr::DefineProperty {
                    object, key, src, ..
                } = instr
                else {
                    return None;
                };
                Some((object, key, src))
            })
            .expect("property definition");
        *[object, key, src][operand] = yield_vm::bytecode::Slot::transient(u16::MAX);
        corrupt.finalize();
        let error = corrupt
            .validate()
            .expect_err("out-of-range definition operand");
        assert!(
            error.contains(&format!("DefineProperty.{message}")),
            "{error}"
        );
    }
    let mut corrupt = build;
    corrupt
        .functions
        .iter_mut()
        .find(|function| function.is_method)
        .expect("method")
        .is_arrow = true;
    corrupt.finalize();
    assert!(
        corrupt
            .validate()
            .expect_err("arrow method")
            .contains("both a method and an arrow")
    );
}

#[test]
fn selects_const_keys_only_for_syntax_known_string_reads() {
    use yield_vm::bytecode::Constant;

    let mut build = compile(
        r#"export function run(o, key) {
            return o.field + o[("\u0066ield")] + o[`field`] + o[key];
        }"#,
        &Compiler::default(),
    )
    .expect("member reads compile");
    let mut keys = Vec::new();
    let mut dynamic = 0;
    for instr in build.functions.iter().flat_map(|function| &function.code) {
        match instr {
            Instr::GetMemberConstKey { key, .. } => keys.push(*key),
            Instr::GetMember { .. } => dynamic += 1,
            Instr::Const { .. } => panic!("the key should not be loaded into a slot"),
            _ => {}
        }
    }
    let key = keys[0] as usize;
    assert_eq!(keys, [key as u32; 3]);
    assert_eq!(dynamic, 1);
    assert_eq!(build.constants[key], Constant::String("field".into()));
    build.validate().expect("valid string key references");

    let missing = build.constants.len() as u32;
    let instr = build
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.code)
        .find(|instr| matches!(instr, Instr::GetMemberConstKey { .. }))
        .expect("static member read");
    let Instr::GetMemberConstKey { key, .. } = instr else {
        unreachable!();
    };
    *key = missing;
    build.finalize();
    assert!(
        build
            .validate()
            .unwrap_err()
            .contains("requires a string constant")
    );
    build.constants.push(Constant::Null);
    build.finalize();
    assert!(
        build
            .validate()
            .unwrap_err()
            .contains("requires a string constant")
    );
}

#[test]
fn lowers_promise_member_call_and_constructor() {
    let source = r#"
function run() {
  const then = Promise.then;
  return new Promise(function (resolve) { return then(resolve); });
}
"#;
    let build = compile(source, &Compiler::default()).expect("promise syntax compiles");
    let code: Vec<_> = build
        .functions
        .iter()
        .flat_map(|function| &function.code)
        .collect();
    let promise = yield_vm::bytecode::BUILTINS
        .iter()
        .position(|name| *name == "Promise")
        .expect("Promise") as u32;
    assert!(
        code.iter().any(|instruction| {
            matches!(instruction, Instr::GetBuiltin { builtin, .. } if *builtin == promise)
        }),
        "Promise loads through GetBuiltin"
    );
    assert!(
        code.iter()
            .any(|instruction| matches!(instruction, Instr::GetMemberConstKey { .. })),
        "Promise.then lowers to GetMemberConstKey"
    );
    assert!(
        code.iter()
            .any(|instruction| matches!(instruction, Instr::CallValue { .. })),
        "calling the loaded member lowers to CallValue"
    );
    assert!(
        code.iter()
            .any(|instruction| matches!(instruction, Instr::Construct { .. })),
        "new Promise lowers to Construct"
    );

    let error = compile(
        "function run() {\n  return new Foo();\n}\n",
        &Compiler::default(),
    )
    .expect_err("new Foo needs a binding");
    assert!(error.message().contains("unknown binding"));
    assert_eq!(error.line(), 2);
    assert!(error.column() > 1);
}

struct TdzCase {
    source: &'static str,
    name: &'static str,
    /// Unique substring. The access identifier starts `skip` bytes into it.
    marker: &'static str,
    skip: usize,
    imports: &'static [(&'static str, &'static [&'static str])],
}

fn access_at(source: &str, marker: &str, skip: usize) -> (u32, usize, usize) {
    let found = source
        .find(marker)
        .unwrap_or_else(|| panic!("missing {marker:?} in {source}"));
    let start = found + skip;
    let prefix = &source[..start];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..start].chars().count() + 1;
    (start as u32, line, column)
}

#[test]
fn rejects_static_use_before_lexical_initialization() {
    let cases = [
        TdzCase {
            source: "function run() {\n  x;\n  let x;\n}\n",
            name: "x",
            marker: "x;",
            skip: 0,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  seed?.();\n  let seed;\n}\n",
            name: "seed",
            marker: "seed?.",
            skip: 0,
            imports: &[("test", &["seed"])],
        },
        TdzCase {
            source: "function run() {\n  x = 1;\n  let x;\n}\n",
            name: "x",
            marker: "x =",
            skip: 0,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  x += 1;\n  let x;\n}\n",
            name: "x",
            marker: "x +=",
            skip: 0,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  x &&= 1;\n  let x;\n}\n",
            name: "x",
            marker: "x &&=",
            skip: 0,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  x();\n  let x;\n}\n",
            name: "x",
            marker: "x()",
            skip: 0,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  typeof x;\n  let x;\n}\n",
            name: "x",
            marker: "typeof x",
            skip: 7,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  typeof inspect;\n  let inspect;\n}\n",
            name: "inspect",
            marker: "typeof inspect",
            skip: 7,
            imports: &[("test", &["inspect"])],
        },
        TdzCase {
            source: "function run() {\n  let x = x;\n}\n",
            name: "x",
            marker: "= x",
            skip: 2,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  const a = b, b = 1;\n}\n",
            name: "b",
            marker: "= b",
            skip: 2,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  let x = 1;\n  {\n    x;\n    let x = 2;\n  }\n}\n",
            name: "x",
            marker: "    x;",
            skip: 4,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  if (false) {\n    x;\n  }\n  let x;\n}\n",
            name: "x",
            marker: "    x;",
            skip: 4,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  for (let a = b, b = 0; false;) {}\n}\n",
            name: "b",
            marker: "= b",
            skip: 2,
            imports: &[],
        },
        TdzCase {
            source: "function run() {\n  return (() => {\n    y;\n    let y;\n  })();\n}\n",
            name: "y",
            marker: "y;",
            skip: 0,
            imports: &[],
        },
    ];

    for case in cases {
        let error = compile(
            case.source,
            &Compiler {
                imports: manifest(case.imports),
                ..Compiler::default()
            },
        )
        .expect_err(case.source);
        let (start, line, column) = access_at(case.source, case.marker, case.skip);
        assert!(
            case.source[start as usize..].starts_with(case.name),
            "marker did not land on {} in {}",
            case.name,
            case.source
        );
        assert_eq!(
            error.message(),
            format!("cannot access {:?} before initialization", case.name),
            "{}",
            case.source
        );
        assert_eq!(error.span().start, start, "{}", case.source);
        assert_eq!(error.line(), line, "{}", case.source);
        assert_eq!(error.column(), column, "{}", case.source);
    }
}

#[test]
fn allows_lexical_use_after_initialization_and_cross_closure_read() {
    let sources = [
        "function run() {\n  let a = 1, b = a;\n  return b;\n}\n",
        "function run() {\n  let x;\n  return x;\n}\n",
        "function run() {\n  return (() => {\n    let y;\n    return y;\n  })();\n}\n",
        "function run() {\n  let x = (() => x)();\n  return x;\n}\n",
        "function run() {\n  let inspect = 1;\n  return typeof inspect;\n}\n",
    ];
    for source in sources {
        let imports = if source.contains("inspect") {
            manifest(&[("test", &["inspect"])])
        } else {
            BTreeMap::new()
        };
        compile(
            source,
            &Compiler {
                imports,
                ..Compiler::default()
            },
        )
        .expect(source);
    }
}

#[test]
fn collects_only_explicit_local_named_exports() {
    let build = compile(
        r#"
export { n as a, n as b, n, n as "" };
export {};
let n = 1;
export var v, initialized = 2;
export let l = 3;
export const c = 4;
export function named() {}
export async function later() {}
function run() {}
"#,
        &Compiler::default(),
    )
    .unwrap();
    assert_eq!(
        entry_module(&build)
            .exports
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "",
            "a",
            "b",
            "c",
            "initialized",
            "l",
            "later",
            "n",
            "named",
            "v"
        ]
    );
    let exports = &entry_module(&build).exports;
    for name in ["", "a", "b"] {
        assert_eq!(exports[name].target, exports["n"].target);
        assert_eq!(exports[name].getter, exports["n"].getter);
    }
    let unexported = compile("function run() {}", &Compiler::default()).unwrap();
    assert!(entry_module(&unexported).exports.is_empty());
}

#[test]
fn rejects_exports_of_unknown_local_bindings() {
    for (source, name) in [
        ("\nexport { missing };", "missing"),
        (
            "{ let privateName = 1; }\nexport { privateName };",
            "privateName",
        ),
    ] {
        let error = compile(source, &Compiler::default()).unwrap_err();
        assert!(
            error.message().contains("unknown export binding"),
            "{error}"
        );
        assert_eq!(error.line(), 2, "{error}");
        assert_eq!(
            error.span().start as usize,
            source.rfind(name).unwrap(),
            "{error}"
        );
    }
}

#[test]
fn host_import_export_aliases_share_one_binding() {
    let build = compile(
        "import { inspect } from 'test';\nexport { inspect, inspect as check };",
        &Compiler {
            imports: manifest(&[("test", &["inspect"])]),
            ..Compiler::default()
        },
    )
    .unwrap();
    let exports = &entry_module(&build).exports;
    assert_eq!(exports["inspect"].target, exports["check"].target);
}

#[test]
fn rejects_duplicate_public_names() {
    for (source, name) in [
        ("export const a = 1;\nexport { a };", "a"),
        ("let a = 1;\nexport { a as one, a as one };", "one"),
    ] {
        let error = compile(source, &Compiler::default()).unwrap_err();
        assert!(
            error.message().to_ascii_lowercase().contains("duplicat"),
            "{error}"
        );
        let span = error.span();
        assert_eq!(
            &source[span.start as usize..span.end as usize],
            name,
            "{error}"
        );
    }
}

#[test]
fn registers_default_exports_and_rejects_their_misuses() {
    // `export { x as default }` keeps the live binding.
    let build = compile(
        "let x = 1;\nexport { x as default, x as alias };",
        &Compiler::default(),
    )
    .unwrap();
    let exports = &entry_module(&build).exports;
    assert_eq!(exports["default"].target, exports["alias"].target);
    // A named default function projects the local binding; the name itself is
    // not auto-exported.
    let build = compile(
        "export default function f() {}\nfunction run() {}",
        &Compiler::default(),
    )
    .unwrap();
    assert!(entry_module(&build).exports.contains_key("default"));
    assert!(!entry_module(&build).exports.contains_key("f"));
    // A named default class shares one binding with the exported local name.
    let build = compile(
        "export default class C {}\nexport { C };",
        &Compiler::default(),
    )
    .unwrap();
    let exports = &entry_module(&build).exports;
    assert_eq!(exports["default"].target, exports["C"].target);
    // A second default is rejected at its source location.
    let error = compile(
        "let x;\nexport { x as default };\nexport default 2;",
        &Compiler::default(),
    )
    .unwrap_err();
    assert!(
        error.message().contains("multiple default exports"),
        "{error}"
    );
    assert_eq!(error.line(), 2, "{error}");
    // An unknown local binding is still rejected, under any public name.
    let error = compile(
        "export default 1;\nexport { missing as x };",
        &Compiler::default(),
    )
    .unwrap_err();
    assert!(
        error.message().contains("unknown export binding"),
        "{error}"
    );
}

#[test]
fn discovers_dynamic_imports_after_source_transforms_and_remaps_after_pruning() {
    struct Source;
    impl SourceTransform for Source {
        fn transform(&mut self, module: &mut ParsedModule<'_>) -> Result<(), TransformError> {
            if module.id() == "main.js" {
                module.replace_source(
                    "import './pruned.js'; if (false) { (() => import('./dep.js')); } \
                     export default import('./dep.js');",
                )?;
            }
            Ok(())
        }
    }

    struct Graph;
    impl GraphTransform for Graph {
        fn transform(&mut self, graph: &mut ModuleGraph<'_>) -> Result<(), TransformError> {
            let entry = graph.entry().clone();
            let deps = graph.dependencies(&entry).unwrap();
            assert_eq!(deps[0].kind, DependencyKind::Static);
            assert_eq!(deps[1].kind, DependencyKind::Dynamic);
            assert_eq!(deps[1].target, "dep.js");
            graph
                .module_mut(&entry)
                .unwrap()
                .replace_source("export function load() { return import('./dep.js'); }")?;
            Ok(())
        }
    }

    let resolver = MemoryResolver::new([
        ("main.js", ""),
        ("pruned.js", ""),
        ("dep.js", "export const value = 42;"),
    ])
    .unwrap();
    let build = Compiler::default()
        .compile(
            Project::new("main.js", resolver)
                .with_source_transform(Source)
                .with_graph_transform(Graph),
        )
        .unwrap();
    assert_eq!(build.modules.len(), 2);
    assert_eq!(entry_module(&build).dependencies, [1]);
    assert_eq!(build.modules[1].id, "dep.js");
    let load = build.functions.iter().find(|f| f.name == "load").unwrap();
    assert!(
        load.code
            .iter()
            .any(|op| matches!(op, Instr::ImportModule { module: 1, .. }))
    );
}

#[test]
fn dynamic_imports_bypass_hosts_even_in_dead_code() {
    let options = Compiler {
        imports: manifest(&[("dep", &[])]),
        ..Compiler::default()
    };
    let error = compile("\nif (false) { () => import('dep'); }", &options).unwrap_err();
    assert_eq!((error.line(), error.column()), (2, 20));
    assert!(error.message().contains("dep"), "{error}");
}

#[test]
fn dynamic_edges_deduplicate_with_static_edges_and_extend_preload_scope() {
    let resolver = MemoryResolver::new([
        (
            "main.js",
            "import './dep.js'; import('./dep.js'); import('././dep.js');",
        ),
        (
            "preload.js",
            "export const shared = 1; if (false) import('./dep.js');",
        ),
        ("dep.js", ""),
    ])
    .unwrap();
    let build = Compiler::default()
        .compile(Project::new("main.js", resolver).with_preload("preload.js"))
        .unwrap();
    let preload = &build.modules[build.preload.unwrap() as usize];
    assert_eq!(preload.dependencies.len(), 1);
    assert_eq!(entry_module(&build).dependencies, preload.dependencies);
    assert_eq!(build.modules[preload.dependencies[0] as usize].id, "dep.js");

    let resolver = MemoryResolver::new([
        ("main.js", "shared;"),
        ("preload.js", "export const shared = 1; import('./dep.js');"),
        ("dep.js", "shared;"),
    ])
    .unwrap();
    let error = Compiler::default()
        .compile(Project::new("main.js", resolver).with_preload("preload.js"))
        .unwrap_err();
    assert_eq!(error.module(), "dep.js");
    assert!(error.message().contains("shared"), "{error}");
}

#[test]
fn graph_transforms_revalidate_dynamic_imports() {
    struct Graph(&'static str);
    impl GraphTransform for Graph {
        fn transform(&mut self, graph: &mut ModuleGraph<'_>) -> Result<(), TransformError> {
            let entry = graph.entry().clone();
            graph.module_mut(&entry).unwrap().replace_source(self.0)?;
            Ok(())
        }
    }

    for (source, message) in [
        ("\nimport('./other.js');", "not in the graph"),
        (
            "\nimport(other);",
            "dynamic import with non-literal specifier",
        ),
    ] {
        let resolver = MemoryResolver::new([("main.js", ""), ("other.js", "")]).unwrap();
        let error = Compiler::default()
            .compile(Project::new("main.js", resolver).with_graph_transform(Graph(source)))
            .unwrap_err();
        assert!(error.message().contains(message), "{error}");
        assert_eq!((error.line(), error.column()), (2, 1));
    }
}

#[test]
fn rejects_unsupported_module_syntax() {
    for (statement, feature) in [
        ("await 1;", "top-level await"),
        ("for await (const value of []) {}", "top-level await"),
        ("import(dep);", "dynamic import with non-literal specifier"),
        (
            "import(`dep`);",
            "dynamic import with non-literal specifier",
        ),
        ("import('dep', {});", "dynamic import phases or options"),
        ("import.source('dep');", "dynamic import phases or options"),
        ("import.meta;", "import.meta"),
        ("import 'dep' with { type: 'json' };", "attributes"),
        ("export * from 'dep' with { type: 'json' };", "attributes"),
    ] {
        let error = compile(&format!("\n{statement}"), &Compiler::default()).unwrap_err();
        assert_eq!(error.line(), 2, "{error}");
        assert!(
            error.message().starts_with("unsupported syntax:"),
            "{error}"
        );
        assert!(error.message().contains(feature), "{error}");
    }
}

#[test]
fn rejects_unreachable_await_using_in_sync_module() {
    for (source, column) in [
        ("\nawait using resource = null;", 1),
        ("\nif (false) { await using resource = null; }", 14),
    ] {
        let error = compile(source, &Compiler::default()).unwrap_err();
        assert_eq!((error.line(), error.column()), (2, column), "{error}");
        assert_eq!(
            error.message(),
            "unsupported syntax: await using outside an async function"
        );
    }
}

#[test]
fn preserves_oxc_resource_diagnostics() {
    for (statement, marker, message) in [
        ("using {x} = {};", "{x}", "may not have binding patterns"),
        ("using resource;", "resource", "must have an initializer"),
        (
            "for (using resource in {}) {}",
            "using",
            "for...in statement",
        ),
        (
            "switch (0) { case 0: using resource = null; }",
            "using",
            "bare case statement",
        ),
        (
            "function f() { if (false) { await using x = null; } }",
            "await",
            "only allowed within async functions",
        ),
    ] {
        let error = compile(&format!("\n{statement}"), &Compiler::default()).unwrap_err();
        assert_eq!(
            (error.line(), error.column()),
            (2, statement.find(marker).unwrap() + 1),
            "{error}"
        );
        assert!(error.message().contains(message), "{error}");
        assert!(
            !error.message().starts_with("unsupported syntax:"),
            "{error}"
        );
    }
}

#[test]
fn accepts_resource_loop_heads() {
    for head in [
        "for (using inner = null; false;) {}",
        "for (await using inner = null; false;) {}",
        "for (using inner of []) {}",
        "for (await using inner of []) {}",
        "for await (using inner of []) {}",
        "for await (await using inner of []) {}",
    ] {
        for outer in ["", "using outer = null;"] {
            let source = format!("export async function run() {{\n  {outer}\n  {head}\n}}");
            compile(&source, &Compiler::default())
                .unwrap_or_else(|error| panic!("{head}: {error}"));
        }
    }
}

#[test]
fn accepts_using_identifier_and_nested_async_resource_declaration() {
    compile(
        "const using = null; export async function run() { await using resource = using; }",
        &Compiler::default(),
    )
    .unwrap();
}

#[test]
fn rejects_unresolved_and_ambiguous_exports_at_the_request() {
    let check = |files: &[(&str, &str)], expected_module, expected_line, expected: &str| {
        let resolver = MemoryResolver::new(files.iter().copied()).unwrap();
        let err = Compiler::default()
            .compile(Project::new("a.js", resolver))
            .unwrap_err();
        assert_eq!(err.module(), expected_module, "{err}");
        assert_eq!(err.line(), expected_line, "{err}");
        assert!(err.message().contains(expected), "{err}");
    };
    check(
        &[
            ("a.js", "import './b.js';"),
            ("b.js", "\nimport { missing } from './a.js';"),
            ("c.js", "not reachable!"),
        ],
        "b.js",
        2,
        "missing",
    );
    check(
        &[
            ("a.js", "\nexport { x } from './b.js';"),
            ("b.js", "export { x } from './a.js';"),
            ("c.js", "export const x = 7;"),
        ],
        "a.js",
        2,
        "x",
    );
    check(
        &[
            ("a.js", "\nimport { x } from './b.js';"),
            ("b.js", "export * from './c.js'; export * from './d.js';"),
            ("c.js", "export let x = 1;"),
            ("d.js", "export let x = 2;"),
        ],
        "a.js",
        2,
        "ambiguous",
    );
}

#[test]
fn rejects_gated_class_features() {
    for (source, feature) in [
        ("class C { x = arguments; }", "arguments"),
        ("class C { static { (() => arguments)(); } }", "arguments"),
    ] {
        let error = compile(source, &Compiler::default()).unwrap_err();
        assert!(error.message().contains(feature), "{error}");
    }
}

#[test]
fn accepts_synchronous_generator_methods() {
    let build = compile(
        "class C { *m() { yield 1; } static *s() { yield 2; } }",
        &Compiler::default(),
    )
    .expect("synchronous generator methods are supported");
    let generators = build
        .functions
        .iter()
        .filter(|function| function.is_generator)
        .count();
    assert_eq!(generators, 2);
}

#[test]
fn rejects_super_calls_outside_derived_constructor_context() {
    for source in [
        "class C extends Object { constructor() { return super; } }",
        "super();",
        "class C { constructor() { super(); } }",
        "class C extends Object { method() { super(); } }",
        "class C extends Object { constructor() { function f() { super(); } } }",
        "class C extends Object { constructor() { function f() { return () => super(); } } }",
        "class C extends Object { x = () => super(); }",
        "class C extends Object { static { super(); } }",
    ] {
        let error = compile(source, &Compiler::default()).unwrap_err();
        assert!(
            error.message().to_ascii_lowercase().contains("super"),
            "{source}: {error}"
        );
    }
}

#[test]
fn rejects_invalid_super_properties_and_optional_links() {
    for source in [
        "super.x;",
        "class C { m() { return super?.x; } }",
        "class C extends Object { constructor() { super?.(); } }",
        "class C { m() { function f() { return () => super.x; } } }",
    ] {
        assert!(compile(source, &Compiler::default()).is_err(), "{source}");
    }
}

#[test]
fn derived_constructor_lowering_captures_context_only_in_lexical_arrows() {
    let build = compile(
        "class Default extends Object {}\n\
         class Explicit extends Object {\n\
           constructor(...args) {\n\
             function ownThis() { return this; }\n\
             const init = () => () => super(...args);\n\
             init()();\n\
             return (() => this)();\n\
           }\n\
         }\n\
         class NoThis extends null { constructor() { return {}; } }",
        &Compiler::default(),
    )
    .unwrap();
    build.validate().unwrap();
    let constructors: Vec<_> = build
        .function(entry_module(&build).evaluation)
        .unwrap()
        .code
        .iter()
        .filter_map(|op| match op {
            Instr::CreateClass {
                constructor,
                default_constructor,
                ..
            } => Some((
                *default_constructor,
                build.function(*constructor).unwrap().this_param.is_some(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(constructors, [(true, false), (false, true), (false, true)]);
    for function in &build.functions {
        for op in &function.code {
            if matches!(
                op,
                Instr::SuperConstruct { .. } | Instr::GetDerivedThis { .. }
            ) {
                assert!(function.is_arrow);
                assert!(function.this_param.is_none());
            }
        }
    }
    let arrow_ops = build
        .functions
        .iter()
        .filter(|f| f.is_arrow)
        .flat_map(|f| &f.code);
    assert!(arrow_ops.clone().any(|op| matches!(
        op,
        Instr::SuperConstruct {
            args: CallArgs::Spread(_),
            ..
        }
    )));
    assert!(
        arrow_ops
            .clone()
            .any(|op| matches!(op, Instr::GetDerivedThis { .. }))
    );
}

#[test]
fn class_lowering_keeps_the_inner_name_out_of_ordinary_bindings() {
    let build = compile(
        "export class C { static read() { return () => C; } } C = null;",
        &Compiler::default(),
    )
    .unwrap();
    build.validate().unwrap();
    let entry = entry_module(&build);
    let script = build.function(entry.evaluation).unwrap();
    let class_ops = script
        .code
        .iter()
        .filter(|op| {
            matches!(
                op,
                Instr::NewClassScope { .. }
                    | Instr::CreateClass { .. }
                    | Instr::ScopedClosure { .. }
                    | Instr::DefineClassMethod { .. }
                    | Instr::InitClassBinding { .. }
            )
        })
        .collect::<Vec<_>>();
    let [
        Instr::NewClassScope {
            dst: scope,
            parent: None,
        },
        Instr::CreateClass {
            scope: Some(constructor_scope),
            constructor,
            ..
        },
        Instr::ScopedClosure {
            scope: method_scope,
            function,
            ..
        },
        Instr::DefineClassMethod {
            is_static: true, ..
        },
        Instr::InitClassBinding {
            scope: init_scope,
            binding,
            ..
        },
    ] = class_ops.as_slice()
    else {
        panic!("unexpected class lowering: {class_ops:?}");
    };
    assert_eq!(scope, constructor_scope);
    assert_eq!(scope, method_scope);
    assert_eq!(scope, init_scope);
    assert!(!build.function(*constructor).unwrap().is_method);
    assert!(build.function(*function).unwrap().is_method);
    let arrow = build
        .functions
        .iter()
        .find(|function| function.is_arrow)
        .unwrap();
    assert!(arrow.code.iter().any(|op| matches!(op,
        Instr::LoadClassBinding { scope: None, binding: inner, .. } if inner == binding
    )));
    for id in [entry.declaration, entry.evaluation] {
        assert!(
            !build
                .function(id)
                .unwrap()
                .code
                .iter()
                .any(|op| matches!(op,
                    Instr::DeclareBinding { binding: outer } if outer == binding
                ))
        );
    }
}

#[test]
fn new_target_is_promoted_or_captured_on_demand() {
    use yield_vm::bytecode::ParamTarget;

    let build = compile(
        r#"
function direct(value = new.target) { return value; }
function capture() { return async () => () => new.target; }
function outer() { function inner() { return new.target; } return inner; }
"#,
        &Compiler::default(),
    )
    .unwrap();
    build.validate().unwrap();
    let function = |name: &str| {
        build
            .functions
            .iter()
            .find(|f| f.name.split('@').next() == Some(name))
            .unwrap()
    };
    assert!(matches!(
        function("direct").new_target_param,
        Some(ParamTarget::Slot(_))
    ));
    assert!(matches!(
        function("capture").new_target_param,
        Some(ParamTarget::Binding(_))
    ));
    assert!(function("outer").new_target_param.is_none());
    assert!(function("inner").new_target_param.is_some());
    assert!(
        build
            .functions
            .iter()
            .filter(|f| f.is_arrow)
            .all(|f| f.new_target_param.is_none())
    );
}

#[test]
fn rejects_new_target_without_an_enclosing_ordinary_function() {
    for statement in [
        "new.target;",
        "const f = () => new.target;",
        "const f = async () => new.target;",
    ] {
        let error = compile(&format!("\n{statement}"), &Compiler::default()).unwrap_err();
        assert_eq!(error.line(), 2, "{error}");
        assert!(error.message().contains("new.target"), "{error}");
    }
}

#[test]
fn lowers_named_default_namespace_and_side_effect_imports() {
    let imports = manifest(&[("host", &["", "default", "fn", "value"])]);
    let build = compile(
        r#"
import fallback, { fn as call, value as item, "" as empty } from "host";
import * as host from "host";
import "host";
function run() { return call(item, fallback, empty, host.value); }
"#,
        &Compiler {
            imports,
            ..Compiler::default()
        },
    )
    .expect("static host imports compile");
    let run = build.functions.iter().find(|f| f.name == "run").unwrap();
    assert!(
        run.code
            .iter()
            .any(|ins| matches!(ins, Instr::CallValue { .. }))
    );
}

#[test]
fn delegates_unknown_host_specifiers_and_rejects_missing_host_exports() {
    let imports = manifest(&[("host", &["known"])]);
    for (source, text) in [
        ("import { known } from 'missing';", "source file not found"),
        ("import { absent } from 'host';", "has no export"),
    ] {
        let error = compile(
            source,
            &Compiler {
                imports: imports.clone(),
                ..Compiler::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.module(), "main.js");
        assert!(error.message().contains(text), "{error}");
        assert!(error.span().start > 0, "{error}");
    }
}

#[test]
fn memory_resolver_resolves_relative_and_root_relative_paths() {
    let resolver = MemoryResolver::new([
        ("src/./main.js", "export const main = 1;"),
        ("src/dep.js", "export const local = 2;"),
        ("dep.js", "export const shared = 3;"),
    ])
    .unwrap();
    let entry = resolver.resolve(None, "./src/main.js").unwrap().id;
    assert_eq!(entry, "src/main.js");
    let local = resolver
        .resolve(Some(&entry), "./dir/../dep.js")
        .unwrap()
        .id;
    assert_eq!(local, "src/dep.js");
    assert_eq!(
        resolver
            .load(&local, &unexpected_load_error)
            .unwrap()
            .source,
        "export const local = 2;"
    );
    let shared = resolver.resolve(Some(&entry), "../dep.js").unwrap().id;
    assert_eq!(shared, "dep.js");
    assert_eq!(resolver.resolve(Some(&entry), "dep.js").unwrap().id, shared);
    assert_eq!(
        resolver
            .load(&shared, &unexpected_load_error)
            .unwrap()
            .source,
        "export const shared = 3;"
    );
    assert!(resolver.resolve(Some(&entry), "../../dep.js").is_err());
}

#[test]
fn memory_resolver_rejects_duplicate_normalized_ids() {
    let error = MemoryResolver::new([("src/dep.js", "1;"), ("src/dir/../dep.js", "2;")])
        .err()
        .expect("normalized IDs should collide");
    assert!(error.contains("duplicate source ID"), "{error}");
    assert!(error.contains("src/dep.js"), "{error}");
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "yield-compiler-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("creating {path:?}: {error}"),
            }
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn node_resolver_resources_require_an_explicit_opted_in_extension() {
    let root = TempDir::new();
    fs::write(root.0.join("theme.css"), "#root { width: 101px }").unwrap();
    let resolver = NodeResolver::web_production(&root.0).unwrap();
    assert!(resolver.resolve(None, "./theme.css").is_err());

    let resolver = resolver.with_loader("css", |id, text| {
        let mut module = LoadedModule::source("export default 'theme';");
        module.resources.push(BuildResource {
            uri: format!("asset:{id}"),
            bytes: text.into_bytes().into(),
        });
        Ok(module)
    });
    assert!(resolver.resolve(None, "./theme").is_err());
    let module = resolver.resolve(None, "./theme.css").unwrap();
    assert_eq!(module.id, "theme.css");
    assert!(!module.is_framework);
    let loaded = resolver.load(&module.id, &unexpected_load_error).unwrap();
    assert_eq!(loaded.source, "export default 'theme';");
    let [resource] = loaded.resources.as_slice() else {
        panic!("expected one resource");
    };
    assert_eq!(resource.uri, "asset:theme.css");
    assert_eq!(&*resource.bytes, b"#root { width: 101px }");
}

#[test]
fn node_resolver_resolves_relative_paths_and_explicit_roots() {
    let root = TempDir::new();
    fs::create_dir(root.0.join("src")).unwrap();
    fs::write(root.0.join("src/main.js"), "export const main = 1;").unwrap();
    fs::write(root.0.join("src/dep.js"), "export const local = 2;").unwrap();
    fs::write(root.0.join("dep.js"), "export const shared = 3;").unwrap();

    let resolver = NodeResolver::new(&root.0).unwrap();
    let entry = resolver.resolve(None, "./src/main.js").unwrap().id;
    assert_eq!(entry, "src/main.js");
    let local = resolver
        .resolve(Some(&entry), "./dir/../dep.js")
        .unwrap()
        .id;
    assert_eq!(local, "src/dep.js");
    assert_eq!(
        resolver
            .load(&local, &unexpected_load_error)
            .unwrap()
            .source,
        "export const local = 2;"
    );
    let shared = resolver.resolve(Some(&entry), "../dep.js").unwrap().id;
    assert_eq!(shared, "dep.js");
    assert!(resolver.resolve(Some(&entry), "dep.js").is_err());
    assert_eq!(resolver.resolve(None, "dep.js").unwrap().id, shared);
    assert_eq!(
        resolver
            .load(&shared, &unexpected_load_error)
            .unwrap()
            .source,
        "export const shared = 3;"
    );
    assert!(resolver.resolve(Some(&entry), "../../dep.js").is_err());
}

#[cfg(unix)]
#[test]
fn node_resolver_canonicalizes_symlink_aliases_and_rejects_escapes() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new();
    let root = dir.0.join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("dep.js"), "1;").unwrap();
    fs::write(dir.0.join("outside.js"), "2;").unwrap();
    symlink("dep.js", root.join("alias.js")).unwrap();
    symlink("../outside.js", root.join("escape.js")).unwrap();

    let resolver = NodeResolver::new(&root).unwrap();
    assert_eq!(resolver.resolve(None, "alias.js").unwrap().id, "dep.js");
    let error = resolver.resolve(None, "escape.js").unwrap_err();
    assert!(error.contains("outside the source root"), "{error}");
    fs::write(root.join("main.js"), "\nimport './escape.js';").unwrap();
    let error = Compiler::default()
        .compile(Project::new("main.js", resolver))
        .unwrap_err();
    assert!(
        error.message().contains("outside the source root"),
        "{error}"
    );
    assert_eq!((error.module(), error.line()), ("main.js", 2));
}

#[test]
fn missing_source_imports_point_to_the_import_specifier() {
    let source = "\n  import { value } from './missing.js';\n";
    let memory = MemoryResolver::new([("src/main.js", source)]).unwrap();
    let root = TempDir::new();
    fs::create_dir(root.0.join("src")).unwrap();
    fs::write(root.0.join("src/main.js"), source).unwrap();
    let file = NodeResolver::new(&root.0).unwrap();
    let file_reason = file
        .resolve(Some(&ModuleId::from("src/main.js")), "./missing.js")
        .unwrap_err();

    for (resolver, reason) in [
        (&memory as &dyn ModuleResolver, "source file not found"),
        (&file as &dyn ModuleResolver, file_reason.as_str()),
    ] {
        let error = Compiler::default()
            .compile(Project::new("./src/main.js", resolver))
            .unwrap_err();
        let (start, line, column) = access_at(source, "'./missing.js'", 0);
        assert_eq!(error.module(), "src/main.js");
        assert_eq!(error.span().start, start, "{error}");
        assert_eq!(error.line(), line, "{error}");
        assert_eq!(error.column(), column, "{error}");
        assert!(error.message().contains("./missing.js"), "{error}");
        assert!(error.message().contains(reason), "{error}");
    }
}

#[test]
fn missing_file_entry_points_to_the_requested_root() {
    let root = TempDir::new();
    let resolver = NodeResolver::new(&root.0).unwrap();
    let error = Compiler::default()
        .compile(Project::new("./missing.js", resolver))
        .unwrap_err();
    assert_eq!(error.module(), "./missing.js");
    assert_eq!(error.line(), 0, "{error}");
    assert_eq!(error.column(), 0, "{error}");
}

fn fixture(root: &TempDir, relative: &str, source: &str) {
    let path = root.0.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

#[test]
fn web_node_resolver_selects_browser_conditions_and_legacy_entries() {
    let root = TempDir::new();
    fixture(
        &root,
        "entry.mjs",
        "import { browser } from 'pkg'; export { browser };",
    );
    fixture(
        &root,
        "node_modules/pkg/package.json",
        r#"{"type":"module","exports":{".":{"default":"./default.js","browser":{"default":"./default.js","production":"./browser.js"}}}}"#,
    );
    fixture(
        &root,
        "node_modules/pkg/browser.js",
        "export const browser = true;",
    );
    fixture(
        &root,
        "node_modules/pkg/default.js",
        "export const fallback = true;",
    );
    fixture(&root, "node_modules/legacy/package.json", "{}");
    fixture(&root, "node_modules/legacy/index.js", "exports.value = 1;");
    fixture(
        &root,
        "node_modules/broken/package.json",
        r#"{"main":"missing.js"}"#,
    );
    fixture(&root, "node_modules/broken/index.js", "exports.value = 2;");
    let web = NodeResolver::web_production(&root.0).unwrap();
    let strict = NodeResolver::new(&root.0).unwrap();
    let entry = web.resolve(None, "./entry.mjs").unwrap().id;
    assert!(
        web.resolve(Some(&entry), "legacy")
            .unwrap()
            .id
            .ends_with("/legacy/index.js")
    );
    assert!(
        strict
            .resolve(Some(&entry), "legacy")
            .unwrap_err()
            .contains("no exports")
    );
    assert!(
        web.resolve(Some(&entry), "pkg/browser.js")
            .unwrap_err()
            .contains("not exported")
    );
    assert!(
        web.resolve(Some(&entry), "broken")
            .unwrap_err()
            .contains("missing.js")
    );
    Compiler::default()
        .compile(Project::new("./entry.mjs", &web))
        .unwrap();
    let error = Compiler::default()
        .compile(Project::new("./entry.mjs", &strict))
        .unwrap_err();
    assert!(error.message().contains("browser"), "{error}");
}

#[test]
fn web_commonjs_links_the_legacy_entry_and_keeps_factory_ids_private() {
    let root = TempDir::new();
    fixture(&root, "entry.mjs", "export { value } from 'legacy';");
    fixture(&root, "node_modules/legacy/package.json", "{}");
    fixture(&root, "node_modules/legacy/index.js", "exports.value = 1;");
    let resolver = NodeResolver::web_production(&root.0).unwrap();
    let build = Compiler::default()
        .compile(Project::new("./entry.mjs", &resolver))
        .unwrap();
    assert!(entry_module(&build).exports.contains_key("value"));
    fixture(
        &root,
        "entry.mjs",
        "import 'cjs-factory:npm:node_modules/legacy/index.js';",
    );
    let error = Compiler::default()
        .compile(Project::new("./entry.mjs", &resolver))
        .unwrap_err();
    assert!(
        error.message().contains("private CommonJS factory"),
        "{error}"
    );
}

#[test]
fn web_commonjs_discovers_only_live_unshadowed_requires() {
    let root = TempDir::new();
    fixture(&root, "entry.mjs", "export { value } from 'pkg';");
    fixture(&root, "node_modules/pkg/package.json", "{}");
    fixture(&root, "node_modules/pkg/prod.js", "exports.value = 1;");
    let path = "node_modules/pkg/index.js";
    fixture(
        &root,
        path,
        r#"
        function shadow(require, process) {
            return process.env.NODE_ENV === 'production' ? 0 : require('local');
        }
        if (process.env.NODE_ENV === 'production') {
            module.exports = 'production' == process.env.NODE_ENV
                ? require('./prod.js') : require('./missing-dev.js');
        } else {
            require('./also-missing-dev.js');
        }
    "#,
    );
    let resolver = NodeResolver::web_production(&root.0).unwrap();
    let compile = || Compiler::default().compile(Project::new("./entry.mjs", &resolver));
    compile().unwrap();
    fixture(&root, "node_modules/pkg/esm.mjs", "export default 1;");
    for (line, marker, reason) in [
        ("require(name)", "require(name)", "one string literal"),
        ("const load = require", "require", "require escape"),
        (
            "exports[name] = 1",
            "exports[name]",
            "indeterminate CommonJS export name",
        ),
        (
            "module.exports = load()",
            "load()",
            "indeterminate CommonJS export shape",
        ),
        (
            "require('./esm.mjs')",
            "require('./esm.mjs')",
            "require of an ES module",
        ),
    ] {
        let source = format!("\n  {line};\n  exports.value = 1;\n");
        fixture(&root, path, &source);
        let error = compile().unwrap_err();
        let (start, line, column) = access_at(&source, marker, 0);
        assert_eq!(error.module(), format!("npm:{path}"));
        assert_eq!(
            (error.span().start, error.line(), error.column()),
            (start, line, column),
            "{error}"
        );
        assert!(
            error.message().starts_with("unsupported syntax:") && error.message().contains(reason),
            "{error}"
        );
    }
    fixture(&root, path, "debugger; exports.value = 1;");
    let error = compile().unwrap_err();
    assert_eq!(error.module(), format!("npm:{path}"));
    assert!(
        error
            .message()
            .contains("post-conversion position: unsupported syntax:"),
        "{error}"
    );
}

#[test]
fn node_resolver_probes_exports_and_package_relative_imports() {
    let root = TempDir::new();
    fixture(
        &root,
        "src/main.js",
        "import { value } from '@scope/pkg'; value;",
    );
    fixture(&root, "src/which.js", "");
    fixture(&root, "src/which.mjs", "");
    fixture(&root, "src/dir/index.mjs", "");
    fixture(
        &root,
        "node_modules/@scope/pkg/package.json",
        r#"{
        "type": "module",
        "metadata": [true, false, null, -1.5e+2, {"extra": "\ud83d\ude00"}],
        "exports": {
            ".": {"default": "./fallback.js", "import": "./\u0065ntry"},
            "./public": {"require": "./private.js", "default": "./dir"},
            "./blocked": null,
            "./nested-block": {"import": {"default": null}, "default": "./private.js"},
            "./wild/*": "./private.js"
        }
    }"#,
    );
    fixture(
        &root,
        "node_modules/@scope/pkg/entry.mjs",
        "export { value } from './private';",
    );
    fixture(
        &root,
        "node_modules/@scope/pkg/private.js",
        "export const value = 1;",
    );
    fixture(&root, "node_modules/@scope/pkg/dir/index.js", "");
    let resolver = NodeResolver::new(&root.0).unwrap();
    let entry = resolver.resolve(None, "src/main").unwrap().id;
    assert_eq!(
        resolver.resolve(Some(&entry), "./which").unwrap().id,
        "src/which.js"
    );
    assert_eq!(
        resolver.resolve(Some(&entry), "./dir").unwrap().id,
        "src/dir/index.mjs"
    );
    let package = resolver.resolve(Some(&entry), "@scope/pkg").unwrap();
    assert_eq!(package.id, "npm:node_modules/@scope/pkg/entry.mjs");
    assert!(
        package.is_framework,
        "installed packages are framework code"
    );
    let private = resolver.resolve(Some(&package.id), "./private").unwrap().id;
    assert_eq!(
        resolver
            .load(&private, &unexpected_load_error)
            .unwrap()
            .source,
        "export const value = 1;"
    );
    assert!(
        resolver
            .resolve(Some(&entry), "@scope/pkg/public")
            .unwrap()
            .id
            .ends_with("dir/index.js")
    );
    for request in [
        "@scope/pkg/private.js",
        "@scope/pkg/blocked",
        "@scope/pkg/nested-block",
        "@scope/pkg/wild/file",
    ] {
        assert!(
            resolver
                .resolve(Some(&entry), request)
                .unwrap_err()
                .contains("not exported")
        );
    }
    for request in [
        "../node_modules/@scope/pkg/private.js",
        "../node_modules/@scope/pkg",
    ] {
        assert!(
            resolver
                .resolve(Some(&entry), request)
                .unwrap_err()
                .contains("node_modules")
        );
    }
    assert!(
        resolver
            .resolve(None, "node_modules/@scope/pkg/private.js")
            .is_err()
    );
    Compiler::default()
        .compile(Project::from_directory(&root.0, "src/main").unwrap())
        .unwrap();
}

#[test]
fn node_resolver_retains_installation_context_for_package_collisions() {
    let root = TempDir::new();
    fixture(&root, "src/main.js", "");
    for prefix in ["node_modules/", "node_modules/dep/node_modules/"] {
        fixture(
            &root,
            &format!("{prefix}dep/package.json"),
            r#"{"exports":"./index.mjs"}"#,
        );
        fixture(&root, &format!("{prefix}dep/index.mjs"), prefix);
        fixture(
            &root,
            &format!("{prefix}sibling/package.json"),
            r#"{"exports":"./index.mjs"}"#,
        );
        fixture(&root, &format!("{prefix}sibling/index.mjs"), prefix);
    }
    let resolver = NodeResolver::new(&root.0).unwrap();
    let entry = resolver.resolve(None, "src/main").unwrap().id;
    let outer = resolver.resolve(Some(&entry), "dep").unwrap().id;
    let inner = resolver.resolve(Some(&outer), "dep").unwrap().id;
    assert_ne!(outer, inner);
    let outer_sibling = resolver.resolve(Some(&entry), "sibling").unwrap().id;
    let inner_sibling = resolver.resolve(Some(&inner), "sibling").unwrap().id;
    assert_ne!(outer_sibling, inner_sibling);
    assert_eq!(
        resolver
            .load(&outer_sibling, &unexpected_load_error)
            .unwrap()
            .source,
        "node_modules/"
    );
    assert_eq!(
        resolver
            .load(&inner_sibling, &unexpected_load_error)
            .unwrap()
            .source,
        "node_modules/dep/node_modules/"
    );
}

#[test]
fn resolvers_report_framework_definitions() {
    let memory = MemoryResolver::new([("src/main.js", ""), ("dep.js", "")]).unwrap();
    assert!(!memory.resolve(None, "src/main.js").unwrap().is_framework);
    assert!(!memory.resolve(None, "dep.js").unwrap().is_framework);

    let root = TempDir::new();
    fixture(&root, "src/main.js", "");
    fixture(
        &root,
        "node_modules/dep/package.json",
        r#"{"exports":"./index.mjs"}"#,
    );
    fixture(&root, "node_modules/dep/index.mjs", "");
    fixture(
        &root,
        "node_modules/dep/node_modules/inner/package.json",
        r#"{"exports":"./index.mjs"}"#,
    );
    fixture(&root, "node_modules/dep/node_modules/inner/index.mjs", "");
    let resolver = NodeResolver::new(&root.0).unwrap();
    let entry = resolver.resolve(None, "src/main").unwrap();
    assert_eq!(entry.id, "src/main.js");
    assert!(
        !entry.is_framework,
        "project files are never framework code"
    );
    let dep = resolver.resolve(Some(&entry.id), "dep").unwrap();
    assert_eq!(dep.id, "npm:node_modules/dep/index.mjs");
    assert!(dep.is_framework, "installed packages are framework code");
    let inner = resolver.resolve(Some(&dep.id), "inner").unwrap();
    assert_eq!(
        inner.id,
        "npm:node_modules/dep/node_modules/inner/index.mjs"
    );
    assert!(
        inner.is_framework,
        "nested dependencies are framework code too"
    );
    let sibling = resolver.resolve(Some(&dep.id), "./index.mjs").unwrap();
    assert_eq!(sibling.id, dep.id);
    assert!(sibling.is_framework);
}

#[test]
fn node_resolver_rejects_unsupported_targets_at_the_import_span() {
    let root = TempDir::new();
    fixture(
        &root,
        "node_modules/legacy/package.json",
        r#"{"main":"./index.mjs"}"#,
    );
    fixture(&root, "node_modules/legacy/index.mjs", "");
    fixture(
        &root,
        "node_modules/untyped/package.json",
        r#"{"exports":"./entry.js"}"#,
    );
    fixture(
        &root,
        "node_modules/untyped/entry.js",
        "module.exports = 1;",
    );
    fixture(
        &root,
        "node_modules/malformed/package.json",
        r#"{"exports":"./entry.mjs",}"#,
    );
    fixture(
        &root,
        "node_modules/pkg/package.json",
        r#"{
        "type": "commonjs",
        "exports": {
            ".": "./entry.js", "./cjs": "./entry.cjs", "./json": "./data",
            "./missing": {"import":"./absent.js", "default":"./okay.mjs"},
            "./directory": "./empty", "./array": ["./okay.mjs"],
            "./escape": "./../legacy/index.mjs", "./okay": "./okay.mjs"
        }
    }"#,
    );
    for path in ["entry.js", "entry.cjs", "data.json", "okay.mjs"] {
        fixture(&root, &format!("node_modules/pkg/{path}"), "");
    }
    fs::create_dir(root.0.join("node_modules/pkg/empty")).unwrap();
    for (request, reason) in [
        ("pkg", "CommonJS"),
        ("pkg/cjs", "CommonJS"),
        ("pkg/json", "JSON"),
        ("pkg/missing", "source file not found"),
        ("pkg/directory", "source file not found"),
        ("pkg/array", "unsupported package export target"),
        ("pkg/escape", "invalid path segment"),
        ("legacy", "no exports"),
        ("node:fs", "Node built-ins"),
        ("fs/promises", "Node built-ins"),
        ("untyped", "CommonJS"),
        ("malformed", "invalid package manifest JSON"),
    ] {
        let source = format!("\n  import '{request}';\n");
        fixture(&root, "main.js", &source);
        let error = Compiler::default()
            .compile(Project::from_directory(&root.0, "main.js").unwrap())
            .unwrap_err();
        let (start, line, column) = access_at(&source, &format!("'{request}'"), 0);
        assert_eq!(error.module(), "main.js");
        assert_eq!(
            (error.span().start, error.line(), error.column()),
            (start, line, column)
        );
        assert!(error.message().contains(reason), "{error}");
    }
    fixture(&root, "main.js", "import 'pkg/okay';");
    Compiler::default()
        .compile(Project::from_directory(&root.0, "main.js").unwrap())
        .unwrap();
}

#[cfg(unix)]
#[test]
fn node_resolver_rejects_symlink_exports_and_relative_package_bypasses() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new();
    let outside = TempDir::new();
    fixture(&outside, "outside.mjs", "");
    fixture(&root, "src/main.js", "");
    fixture(&root, "src/inside.mjs", "");
    fixture(
        &root,
        "node_modules/pkg/package.json",
        r#"{
        "exports": {".": "./good.mjs", "./outside": "./outside.mjs", "./escape": "./escape.mjs"}
    }"#,
    );
    fixture(&root, "node_modules/pkg/actual.mjs", "");
    symlink("actual.mjs", root.0.join("node_modules/pkg/good.mjs")).unwrap();
    symlink(
        outside.0.join("outside.mjs"),
        root.0.join("node_modules/pkg/outside.mjs"),
    )
    .unwrap();
    symlink(
        "../../src/inside.mjs",
        root.0.join("node_modules/pkg/escape.mjs"),
    )
    .unwrap();
    symlink(
        "../node_modules/pkg/actual.mjs",
        root.0.join("src/alias.mjs"),
    )
    .unwrap();
    let resolver = NodeResolver::new(&root.0).unwrap();
    let entry = resolver.resolve(None, "src/main.js").unwrap().id;
    let package = resolver.resolve(Some(&entry), "pkg").unwrap().id;
    assert_eq!(package, "npm:node_modules/pkg/actual.mjs");
    assert_eq!(
        resolver.resolve(Some(&package), "./good.mjs").unwrap().id,
        package
    );
    assert!(
        resolver
            .resolve(Some(&entry), "pkg/outside")
            .unwrap_err()
            .contains("outside the source root")
    );
    assert!(
        resolver
            .resolve(Some(&entry), "pkg/escape")
            .unwrap_err()
            .contains("package boundary")
    );
    assert!(
        resolver
            .resolve(Some(&entry), "./alias.mjs")
            .unwrap_err()
            .contains("package boundary")
    );
}

#[test]
fn imported_bindings_are_readonly_and_shadow_normally() {
    let imports = manifest(&[("host", &["value"])]);
    let build = compile(
        "import { value } from 'host'; value = 1;",
        &Compiler {
            imports: imports.clone(),
            ..Compiler::default()
        },
    )
    .expect("imports are const; the assignment throws at runtime");
    assert!(
        build.functions.iter().any(|function| function
            .code
            .iter()
            .any(|ins| matches!(ins, Instr::ThrowAssignmentToConst))),
        "assigning an import must lower to a const violation"
    );
    compile(
        "import { value } from 'host'; function run() { let value = 1; return value; }",
        &Compiler {
            imports,
            ..Compiler::default()
        },
    )
    .expect("local shadowing allowed");
}

#[test]
fn imported_values_in_closures_are_direct_operands_not_environment_cells() {
    let build = compile(
        "import { value } from 'host'; function run() { return value; }",
        &Compiler {
            imports: manifest(&[("host", &["value"])]),
            ..Compiler::default()
        },
    )
    .expect("imported closure reference compiles");
    let run = build
        .functions
        .iter()
        .find(|function| function.name == "run")
        .unwrap();
    assert!(
        run.code
            .iter()
            .any(|ins| matches!(ins, Instr::GetImport { .. }))
    );
    assert!(
        !run.code
            .iter()
            .any(|ins| matches!(ins, Instr::GetBinding { .. }))
    );
}

#[test]
fn imported_assignments_use_const_throws_without_binding_cells() {
    let build = compile(
        "import { value } from 'host'; value += 1;",
        &Compiler {
            imports: manifest(&[("host", &["value"])]),
            ..Compiler::default()
        },
    )
    .expect("readonly import assignment lowers");
    let entry = entry_module(&build);
    let script = build.function(entry.evaluation).unwrap();
    assert!(
        script
            .code
            .iter()
            .any(|ins| matches!(ins, Instr::GetImport { .. }))
    );
    assert!(
        script
            .code
            .iter()
            .any(|ins| matches!(ins, Instr::ThrowAssignmentToConst))
    );
    assert!(build.module_bindings(build.entry).next().is_none());
    assert!(
        !script
            .code
            .iter()
            .any(|ins| matches!(ins, Instr::DeclareBinding { .. } | Instr::SetBinding { .. }))
    );
}

#[test]
fn interns_repeated_string_constants() {
    use yield_vm::bytecode::Constant;
    let build = compile(
        "function run() { return { foo: \"foo\", value: \"foo\" }.foo; }",
        &Compiler::default(),
    )
    .expect("source compiles");
    let matches = build
        .constants
        .iter()
        .filter(|constant| matches!(constant, Constant::String(value) if value == "foo"))
        .count();
    assert_eq!(matches, 1);
}

#[test]
fn emits_utf16_for_string_literals_templates_and_property_keys() {
    use yield_vm::bytecode::Constant;

    let build = compile(
        concat!(
            r#"function run(x) { return { '\uD800': '\u{1F600}\uD801', ['\uD802']: `\uD803"#,
            "\r\n",
            r#"${x}\u{DC00}` }; }"#,
        ),
        &Compiler::default(),
    )
    .expect("source compiles");
    for expected in [
        &[0xd800][..],
        &[0xd83d, 0xde00, 0xd801],
        &[0xd802],
        &[0xd803, 0x000a],
        &[0xdc00],
    ] {
        assert!(
            build.constants.iter().any(|constant| {
                matches!(constant, Constant::String(value) if value.units().eq(expected.iter().copied()))
            }),
            "missing UTF-16 constant {expected:x?}"
        );
    }
}

#[test]
fn regexp_literal_lowers_without_reading_the_constructor_binding() {
    let function = compile_run(r"function run(RegExp) { return /a\/b/ig; }");
    assert!(function.code.iter().any(|instruction| matches!(
        instruction,
        Instr::NewRegExp { source, flags, .. } if source == r"a\/b" && flags == "gi"
    )));
    assert!(!function.code.iter().any(|instruction| matches!(
        instruction,
        Instr::GetBuiltin { .. } | Instr::Construct { .. }
    )));
}

#[test]
fn accepts_ecmascript_regexp_features_and_locates_invalid_literals() {
    for literal in [r"/(a)\1/", "/(?=a)a/", "/a/d"] {
        compile(&format!("{literal};"), &Compiler::default()).unwrap();
    }
    let error = compile("\n  /(/;", &Compiler::default()).unwrap_err();
    assert_eq!((error.line(), error.column()), (2, 3), "{error}");
    assert!(
        error
            .message()
            .starts_with("unsupported syntax: RegExp literal:"),
        "{error}"
    );
}

#[test]
fn exported_lexical_declarations_keep_the_tdz() {
    for declaration in ["let", "const"] {
        let source = format!("\n  x;\nexport {declaration} x = 1;");
        let error = compile(&source, &Compiler::default()).unwrap_err();
        assert!(error.message().contains("before initialization"), "{error}");
        assert_eq!((error.line(), error.column()), (2, 3), "{error}");
    }
}

#[test]
fn names_nested_in_exported_patterns_are_public() {
    let build = compile(
        "export const { x: [a], ...rest } = { x: [1] };",
        &Compiler::default(),
    )
    .unwrap();
    assert_eq!(
        entry_module(&build)
            .exports
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["a", "rest"]
    );
}

#[test]
fn host_declarations_supply_the_import_manifest() {
    let dts = r#"
declare module "a" {
    interface Box<T> { value: T }
    type Alias<T> = Box<T>;
    export function id<T extends Box<string>>(x: T): T;
}
"#;
    let options = Compiler::from_dts(dts).unwrap();
    compile(
        "import { id } from 'a';\nexport function run() { return id(1); }",
        &options,
    )
    .unwrap();

    for (name, source) in [
        ("Box", "import { Box } from 'a';\nexport function run() {}"),
        (
            "Alias",
            "import { Alias } from 'a';\nexport function run() {}",
        ),
        (
            "seed",
            "import { seed } from 'a';\nexport function run() {}",
        ),
    ] {
        let error = compile(source, &options).unwrap_err();
        assert_eq!(
            error.message(),
            format!("host module \"a\" has no export {name:?}")
        );
    }

    let error =
        Compiler::from_dts("declare module \"a\" {\n  export * from \"./b\";\n}").unwrap_err();
    assert_eq!(error.message(), "unsupported syntax: `export *`");
    assert_eq!(error.line(), 2);
}

#[test]
fn host_declaration_type_only_module_keeps_an_empty_manifest_entry() {
    let options =
        Compiler::from_dts("declare module \"types\" { export interface Shape { a: 1 } }").unwrap();
    assert!(options.imports["types"].is_empty());
    compile("export function run() {}", &options).unwrap();
}

fn compile_generator(source: &str) -> yield_vm::bytecode::Function {
    let build = compile(source, &Compiler::default()).expect("source compiles");
    build
        .functions
        .into_iter()
        .find(|function| function.name == "run")
        .expect("missing run")
}

#[test]
fn generator_entries_retain_only_values_their_path_reads() {
    let both = compile_generator(
        r#"
export function* run(a, b, c) {
  try {
    yield a;
  } catch (error) {
    return b;
  }
  return c;
}
"#,
    );
    assert!(both.is_generator);
    let [entry] = both.generator_resumes.as_slice() else {
        panic!("expected one generator entry");
    };
    let saved = entry.saved_slots.len();
    assert_eq!(saved, 2, "the throw path keeps b and the next path keeps c");

    // Each value is read on exactly one of the three resumed paths.
    for (source, expected) in [
        (
            r#"
export function* run(a, b, c) {
  try {
    yield a;
  } catch (error) {
    return 0;
  }
  return c;
}
"#,
            1,
        ),
        (
            r#"
export function* run(a, b, c) {
  try {
    yield a;
  } catch (error) {
    return b;
  }
  return 0;
}
"#,
            1,
        ),
        (
            r#"
export function* run(a, b, c) {
  try {
    yield a;
  } catch (error) {
    return 0;
  }
  return 0;
}
"#,
            0,
        ),
    ] {
        let function = compile_generator(source);
        assert_eq!(
            function.generator_resumes[0].saved_slots.len(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn generator_entry_of_one_yield_keeps_a_slot_another_yield_injects() {
    // `x` is the destination of the first resume and is read after the second
    // suspension, so subtracting destinations from the joined set would drop it.
    let subject = compile_generator(
        r#"
export function* run(a) {
  const x = yield a;
  try {
    return 1;
  } finally {
    yield 0;
    a(x);
  }
}
"#,
    );
    let baseline = compile_generator(
        r#"
export function* run(a) {
  const x = yield a;
  try {
    return 1;
  } finally {
    yield 0;
    a(0);
  }
}
"#,
    );
    assert_eq!(subject.generator_resumes.len(), 2);
    assert_eq!(baseline.generator_resumes.len(), 2);
    let delta = subject.generator_resumes[1].saved_slots.len() as i64
        - baseline.generator_resumes[1].saved_slots.len() as i64;
    assert_eq!(delta, 1, "the finalizer reads a value declared before it");
}

#[test]
fn empty_if_leaves_no_branch_or_jump() {
    let build = compile(
        "export function run(flag) { if (flag) {} else {} return 7; }",
        &Compiler::default(),
    )
    .unwrap();
    build.validate().unwrap();
    let run = build.functions.iter().find(|f| f.name == "run").unwrap();
    assert!(!run.code.iter().any(|instr| matches!(
        instr,
        Instr::BranchFalse { .. } | Instr::BranchTrue { .. } | Instr::Jump { .. }
    )));
}

#[test]
fn pruned_suspensions_do_not_leave_resume_entries() {
    let build = compile(
        r#"export async function* run(flag) {
            let enabled;
            if (flag) enabled = false;
            else enabled = false;
            if (enabled) yield await 1;
            yield await 2;
        }"#,
        &Compiler::default(),
    )
    .expect("async generator compiles");
    build
        .validate()
        .expect("remaining entries and targets are valid");
    let generator = build.functions.iter().find(|f| f.is_generator).unwrap();
    let reference = compile(
        "export async function* run() { yield await 2; }",
        &Compiler::default(),
    )
    .expect("reference generator compiles");
    let reference = reference.functions.iter().find(|f| f.is_generator).unwrap();
    assert_eq!(generator.resumes.len(), reference.resumes.len());
    assert_eq!(generator.generator_resumes.len(), 1);
}

#[test]
fn generator_dead_parameter_is_not_saved() {
    let with_dead = compile_generator(
        r#"
export function* run(a, dead) {
  yield a;
}
"#,
    );
    let without_dead = compile_generator(
        r#"
export function* run(a) {
  yield a;
}
"#,
    );
    assert_eq!(
        with_dead.generator_resumes[0].saved_slots,
        without_dead.generator_resumes[0].saved_slots
    );
    assert_eq!(with_dead.persistent_slots, without_dead.persistent_slots);
}

#[test]
fn yield_delegation_uses_one_delegated_suspension() {
    let function = compile_generator(
        r#"
export function* run(iterable) {
  return yield* iterable;
}
"#,
    );
    assert_eq!(function.generator_resumes.len(), 1);
    assert_eq!(
        function
            .code
            .iter()
            .filter(|instruction| matches!(instruction, Instr::Yield { delegate: true, .. }))
            .count(),
        1
    );
    assert!(
        function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instr::IteratorCall { .. })),
        "delegation calls the delegate's methods directly"
    );
}

#[test]
fn lowers_async_generators_with_both_resume_tables() {
    let build = compile(
        "const holder = { async *m() { yield await 1; } };",
        &Compiler::default(),
    )
    .expect("async generators are supported");
    let function = build
        .functions
        .iter()
        .find(|function| function.is_generator)
        .expect("async generator method");
    assert!(function.is_async);
    assert!(function.generator_start.is_some());
    assert_eq!(function.generator_resumes.len(), 1);
    // The explicit `await`, the yielded operand, and the return entry's own
    // assimilation each take a resume point.
    assert_eq!(function.resumes.len(), 3);
    build
        .validate()
        .expect("an async generator carries both resume tables");
}

#[test]
fn build_loading_rejects_illegal_generator_tables() {
    let source = r#"
export function* run(a) {
  try {
    return a;
  } finally {
    yield 1;
  }
}
"#;
    let index = |build: &Build| {
        build
            .functions
            .iter()
            .position(|function| function.name == "run")
            .expect("missing run")
    };
    type Mutation = fn(&mut yield_vm::Function);
    let mutations: [(&str, Mutation); 4] = [
        ("is not the instruction after its yield", |function| {
            function.generator_resumes[0].next_pc += 1;
        }),
        ("does not read the return payload", |function| {
            function.generator_resumes[0].return_pc = 0;
        }),
        ("saved_slots are not strictly ascending", |function| {
            let saved = &mut function.generator_resumes[0].saved_slots;
            saved.push(saved[0]);
        }),
        ("does not follow its start boundary", |function| {
            function
                .generator_start
                .as_mut()
                .expect("generator start")
                .body_pc += 1;
        }),
    ];
    for (message, mutate) in mutations {
        let mut build = compile(source, &Compiler::default()).expect("source compiles");
        let position = index(&build);
        mutate(&mut build.functions[position]);
        build.finalize();
        let error = build.validate().expect_err(message);
        assert!(error.contains(message), "{message}: {error}");
    }
}

#[test]
fn generator_resume_targets_participate_in_build_identity() {
    let mut build = compile(
        r#"
export function* run(a) {
  yield a;
}
"#,
        &Compiler::default(),
    )
    .expect("source compiles");
    let before = build.build_id.clone();
    let position = build
        .functions
        .iter()
        .position(|function| function.name == "run")
        .expect("missing run");
    let other = build
        .functions
        .iter()
        .filter(|function| function.is_generator)
        .count();
    assert_eq!(other, 1, "only the body generator resumes");
    build.functions[position].generator_resumes[0]
        .throw_dst
        .index += 1;
    build.finalize();
    assert_ne!(build.build_id, before);
    assert_ne!(build.compute_id(), before);
}

#[test]
fn typeof_missing_global_prunes_dead_function_creation() {
    let options = Compiler::default();
    let guarded = compile(
        "export function run() { if (typeof Device < 'u') { var saved = () => new Device(); } return saved; }",
        &options,
    )
    .expect("the dead side is not lowered");
    let baseline = compile(
        "export function run() { var saved; return saved; }",
        &options,
    )
    .expect("baseline compiles");

    assert_eq!(guarded.functions.len(), baseline.functions.len());
    assert!(guarded.functions.iter().all(|function| {
        !function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instr::BranchFalse { .. }))
    }));
}

#[test]
fn typeof_pruning_covers_expression_forms_and_checks_dead_syntax() {
    let source = r#"
export function run() {
  var a;
  if ('u' > typeof Device) { a = () => new Device(); }
  var b = typeof Device === 'undefined' ? 1 : (() => Missing())();
  var c = typeof Device === 'object' && (() => new Device());
  var d = typeof Device || (() => Missing())();
  return [a, b, c, d];
}
"#;
    let guarded = compile(source, &Compiler::default()).expect("guards lower");
    let baseline = compile(
        "export function run() { var a, b, c, d; return [a, b, c, d]; }",
        &Compiler::default(),
    )
    .expect("baseline compiles");
    assert_eq!(
        guarded.functions.len(),
        baseline.functions.len(),
        "dead functions are not enqueued"
    );

    let error = compile(
        "export function run() { if (typeof Device == 'object') import(unsupported); }",
        &Compiler::default(),
    )
    .expect_err("discovery must validate dead syntax");
    assert!(error.message().contains("dynamic import"), "{error}");
    assert_eq!((error.line(), error.column()), (1, 56));
}

#[test]
fn typeof_probe_does_not_cross_bindings_modules_or_tdz() {
    let options = Compiler::default();
    let parameter_shadow = compile(
        "function local(Device) { return typeof Device; } export function run() { return Device; }",
        &options,
    )
    .expect_err("a local parameter must not authorize another function");
    assert!(
        parameter_shadow
            .message()
            .contains("unknown binding \"Device\"")
    );

    let tdz = compile("typeof x; let x;", &options)
        .expect_err("typeof preserves the existing same-function TDZ error");
    assert!(tdz.message().contains("before initialization"), "{tdz}");

    let reachable = compile(
        "export function run() { typeof Device; return Missing(); }",
        &options,
    )
    .expect_err("an unrelated unknown callee remains a compile error");
    assert!(reachable.message().contains("unknown callee \"Missing\""));
}

#[test]
fn typeof_probe_does_not_leak_between_modules() {
    let resolver = MemoryResolver::new([
        (
            "main.js",
            "import { probe } from './probe.js'; export function run() { return Device; }",
        ),
        (
            "probe.js",
            "export function probe() { return typeof Device; }",
        ),
    ])
    .expect("valid module paths");
    let error = Compiler::default()
        .compile(Project::new("main.js", resolver))
        .expect_err("the imported module's probe cannot authorize the entry");
    assert!(
        error.message().contains("unknown binding \"Device\""),
        "{error}"
    );
}

#[test]
fn folds_primitive_tests_and_number_operations() {
    let build = compile(
        r#"
export function run() {
  const nan = 0 / 0;
  const zero = -0;
  if (nan === nan || zero !== 0 || 1n !== 0x1n || 1n === 1) return 99;
  return (2 + 3) * 4 - 1;
}
"#,
        &Compiler::default(),
    )
    .unwrap();
    let run = build.functions.iter().find(|f| f.name == "run").unwrap();
    let [Instr::Const { constant, .. }, Instr::Return { .. }] = run.code.as_slice() else {
        panic!("expected a literal return, got {:?}", run.code);
    };
    assert_eq!(
        build.constants[*constant as usize],
        yield_vm::bytecode::Constant::Number(19.0)
    );
}

#[test]
fn selects_constant_rhs_after_a_join() {
    use yield_vm::bytecode::BinaryOp;

    let run = compile_run(
        r#"
export function run(value, flag) {
  let rhs;
  if (flag) rhs = 1 + 2;
  else rhs = 3;
  if (value < rhs) return value + rhs;
  return value;
}
"#,
    );
    assert!(run.code.iter().any(|op| matches!(
        op,
        Instr::BranchCompareConstFalse {
            op: BinaryOp::Lt,
            ..
        }
    )));
    assert!(run.code.iter().any(|op| matches!(
        op,
        Instr::BinaryConst {
            op: BinaryOp::Add,
            ..
        }
    )));
}

#[test]
fn signed_zero_facts_do_not_merge_at_a_join() {
    use yield_vm::bytecode::{BinaryOp, Constant};

    let build = compile(
        r#"
export function run(flag) {
  let rhs;
  if (flag) rhs = 0;
  else rhs = -0;
  return 1 / rhs;
}
"#,
        &Compiler::default(),
    )
    .unwrap();
    let run = build.functions.iter().find(|f| f.name == "run").unwrap();
    for bits in [0.0_f64.to_bits(), (-0.0_f64).to_bits()] {
        assert!(run.code.iter().any(|op| matches!(op, Instr::Const { constant, .. }
            if matches!(build.constants[*constant as usize], Constant::Number(n) if n.to_bits() == bits))));
    }
    assert!(run.code.iter().any(|op| matches!(
        op,
        Instr::Binary {
            op: BinaryOp::Div,
            ..
        }
    )));
}

#[test]
fn stack_sites_follow_ecmascript_lines_and_parameter_initializers() {
    let source = "/*a*/\r\n/*b*/\r/*c*/\n/*d*/\u{2028}/*e*/\u{2029}export function run(x = /*😀é*/ Error('x')) { return x; }";
    let build = compile(source, &Compiler::default()).unwrap();
    let run = build.functions.iter().find(|f| f.name == "run").unwrap();
    let pc = run
        .code
        .iter()
        .position(|op| matches!(op, Instr::CallValue { .. }))
        .unwrap() as u32;
    let site = run
        .stack_sites
        .iter()
        .rev()
        .find(|site| site.pc <= pc)
        .unwrap();
    assert_eq!((site.line, site.column), (6, 33));
}

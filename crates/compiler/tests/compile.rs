use std::collections::{BTreeMap, BTreeSet};
use yield_compiler::{CompileOptions, compile};
use yield_vm::bytecode::{CallArgs, Instr};

#[test]
fn compiles_overflowing_number_literals() {
    let build = compile(
        "export function run() { return [1e999, -1e999]; }",
        &CompileOptions::default(),
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
    let options = CompileOptions::default();
    let build = yield_compiler::compile_with_preload(
        "export function shared() { return 'same'; }",
        "export function shared() { return 'same'; }",
        &options,
    )
    .unwrap();
    let preload = build.preload.as_ref().unwrap();
    assert_eq!(build.script, 0);
    assert_eq!(preload.initializer, 1);
    assert_ne!(build.exports["shared"], preload.exports["shared"]);
    assert_eq!(build.constants.len(), 1);

    let error = yield_compiler::compile_with_preload("", "\nmissing;", &options).unwrap_err();
    assert!(error.message().starts_with("preload: "), "{error}");
    assert_eq!(error.line(), 2);

    let error = yield_compiler::compile_with_preload("\nmissing;", "", &options).unwrap_err();
    assert!(!error.message().starts_with("preload: "), "{error}");
    assert_eq!(error.line(), 2);
}

#[test]
fn validates_preload_initializer_and_its_own_exports() {
    let build = yield_compiler::compile_with_preload(
        "export let main = 0; export function run() {}",
        "export let n = 1;",
        &CompileOptions::default(),
    )
    .unwrap();
    let mut changed = build.clone();
    changed.preload.as_mut().unwrap().initializer = changed.script;
    assert!(changed.validate().unwrap_err().contains("build_id"));
    changed.finalize();
    assert!(changed.validate().unwrap_err().contains("distinct"));

    let mut changed = build.clone();
    changed.preload.as_mut().unwrap().initializer = u32::MAX;
    changed.finalize();
    assert!(changed.validate().unwrap_err().contains("does not exist"));

    let mut changed = build.clone();
    changed
        .preload
        .as_mut()
        .unwrap()
        .exports
        .insert("n".into(), changed.exports["main"]);
    changed.finalize();
    assert!(
        changed
            .validate()
            .unwrap_err()
            .contains("not declared in the preload")
    );

    let mut changed = build;
    let initializer = changed.preload.as_ref().unwrap().initializer;
    let main = changed
        .functions
        .iter_mut()
        .find(|f| f.func_id == changed.script)
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
            .contains("closes over the preload")
    );
}

#[test]
fn rejects_preload_getters_reading_the_wrong_binding() {
    let mut build = yield_compiler::compile_with_preload(
        "",
        "export let x = 1, y = 2;",
        &CompileOptions::default(),
    )
    .unwrap();
    let preload = build.preload.as_mut().unwrap();
    preload.getters.insert("x".into(), preload.getters["y"]);
    assert!(build.validate().unwrap_err().contains("build_id"));
    build.finalize();
    assert!(
        build
            .validate()
            .unwrap_err()
            .contains("invalid preload getter")
    );
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
        &CompileOptions {
            imports: manifest(&[("test", &["inspect", "seed", "wait"])]),
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
        &CompileOptions::default(),
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
        &CompileOptions::default(),
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
fn rejects_unsupported_syntax_with_location() {
    let error = compile("function run() { tag`value`; }", &CompileOptions::default())
        .expect_err("tagged templates are outside the PoC subset");
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
        let error = compile(source, &CompileOptions::default()).expect_err(source);
        assert!(
            error.message().to_lowercase().contains(message),
            "{source}: {error}"
        );
        assert_eq!(error.line(), 1, "{source}: {error}");
        assert!(error.column() > 1, "{source}: {error}");
    }
}

#[test]
fn rejects_for_await_of_with_location() {
    let source = "async function run() {\n  for await (const x of []) {}\n}";
    let error = compile(source, &CompileOptions::default()).unwrap_err();
    assert!(
        error.message().contains("unsupported syntax: for-await-of"),
        "{error}"
    );
    assert_eq!(error.line(), 2);
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
        &CompileOptions::default(),
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
        let error = compile(&source, &CompileOptions::default()).expect_err(statement);
        assert!(error.message().contains(diagnostic), "{statement}: {error}");
        assert_eq!(error.line(), 2, "{statement}: {error}");
    }
}

#[test]
fn rejects_delete_of_identifier_in_strict_modules() {
    for operand in ["x", "((x))"] {
        let source = format!("function run(x) {{\n  return delete {operand};\n}}");
        let error = compile(&source, &CompileOptions::default()).expect_err(&source);
        assert_eq!(error.line(), 2);
        assert!(error.message().contains("delete"), "{error}");
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
        let error = compile(source, &CompileOptions::default()).expect_err(source);
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
            compile(source, &CompileOptions::default()).expect_err("label misuse must be rejected");
        assert!(error.line() >= 1, "{source}: {error}");
    }
}

#[test]
fn compiles_object_spread() {
    let build = compile(
        "function run(other) { return { before: 1, ...other, after: 2 }; }",
        &CompileOptions::default(),
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
        &CompileOptions::default(),
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
fn lowers_promise_member_call_and_constructor() {
    let source = r#"
function run() {
  const then = Promise.then;
  return new Promise(function (resolve) { return then(resolve); });
}
"#;
    let build = compile(source, &CompileOptions::default()).expect("promise syntax compiles");
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
            .any(|instruction| matches!(instruction, Instr::GetMember { .. })),
        "Promise.then lowers to GetMember"
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
        &CompileOptions::default(),
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
            &CompileOptions {
                imports: manifest(case.imports),
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
        compile(source, &CompileOptions { imports }).expect(source);
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
        &CompileOptions::default(),
    )
    .unwrap();
    assert_eq!(
        build.exports.keys().map(String::as_str).collect::<Vec<_>>(),
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
    for name in ["", "a", "b"] {
        assert_eq!(build.exports[name], build.exports["n"]);
    }
    assert!(
        compile("function run() {}", &CompileOptions::default())
            .unwrap()
            .exports
            .is_empty()
    );
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
        let error = compile(source, &CompileOptions::default()).unwrap_err();
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
        &CompileOptions {
            imports: manifest(&[("test", &["inspect"])]),
        },
    )
    .unwrap();
    assert_eq!(build.exports["inspect"], build.exports["check"]);
}

#[test]
fn rejects_duplicate_public_names() {
    for (source, name) in [
        ("export const a = 1;\nexport { a };", "a"),
        ("let a = 1;\nexport { a as one, a as one };", "one"),
    ] {
        let error = compile(source, &CompileOptions::default()).unwrap_err();
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
fn rejects_unsupported_module_syntax() {
    for (statement, feature) in [
        ("await 1;", "top-level await"),
        ("for await (const value of []) {}", "top-level await"),
        ("import 'dep';", "unknown host module"),
        ("import('dep');", "dynamic import"),
        ("import.meta;", "import.meta"),
        ("export default 1;", "default export"),
        ("let x; export { x as default };", "default export"),
        ("export { x } from 'dep';", "re-export"),
        ("export * from 'dep';", "re-export"),
        ("export * as ns from 'dep';", "re-export"),
    ] {
        let error = compile(&format!("\n{statement}"), &CompileOptions::default()).unwrap_err();
        assert_eq!(error.line(), 2, "{error}");
        assert!(
            error.message().starts_with("unsupported syntax:"),
            "{error}"
        );
        assert!(error.message().contains(feature), "{error}");
    }
}

#[test]
fn rejects_gated_class_features() {
    for (source, feature) in [
        ("class C { x = arguments; }", "arguments"),
        ("class C { static { (() => arguments)(); } }", "arguments"),
        ("class C { *m() {} }", "generator"),
    ] {
        let error = compile(source, &CompileOptions::default()).unwrap_err();
        assert!(error.message().contains(feature), "{error}");
    }
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
        let error = compile(source, &CompileOptions::default()).unwrap_err();
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
        assert!(
            compile(source, &CompileOptions::default()).is_err(),
            "{source}"
        );
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
        &CompileOptions::default(),
    )
    .unwrap();
    build.validate().unwrap();
    let constructors: Vec<_> = build.functions[0]
        .code
        .iter()
        .filter_map(|op| match op {
            Instr::CreateClass {
                constructor,
                default_constructor,
                ..
            } => Some((
                *default_constructor,
                build.functions[*constructor as usize].this_param.is_some(),
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
        &CompileOptions::default(),
    )
    .unwrap();
    build.validate().unwrap();
    let script = &build.functions[build.script as usize];
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
    assert!(!build.functions[*constructor as usize].is_method);
    assert!(build.functions[*function as usize].is_method);
    let arrow = build
        .functions
        .iter()
        .find(|function| function.is_arrow)
        .unwrap();
    assert!(arrow.code.iter().any(|op| matches!(op,
        Instr::LoadClassBinding { scope: None, binding: inner, .. } if inner == binding
    )));
    assert!(!script.code.iter().any(|op| matches!(op,
        Instr::DeclareBinding { binding: outer } if outer == binding
    )));
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
        &CompileOptions::default(),
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
        let error = compile(&format!("\n{statement}"), &CompileOptions::default()).unwrap_err();
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
        &CompileOptions { imports },
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
fn rejects_unknown_host_module_and_export_at_import_location() {
    let imports = manifest(&[("host", &["known"])]);
    for (source, text) in [
        ("import { known } from 'missing';", "unknown host module"),
        ("import { absent } from 'host';", "has no export"),
    ] {
        let error = compile(
            source,
            &CompileOptions {
                imports: imports.clone(),
            },
        )
        .unwrap_err();
        assert!(error.message().contains(text), "{error}");
        assert!(error.span().start > 0, "{error}");
    }
}

#[test]
fn imported_bindings_are_readonly_and_shadow_normally() {
    let imports = manifest(&[("host", &["value"])]);
    let build = compile(
        "import { value } from 'host'; value = 1;",
        &CompileOptions {
            imports: imports.clone(),
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
        &CompileOptions { imports },
    )
    .expect("local shadowing allowed");
}

#[test]
fn imported_values_in_closures_are_direct_operands_not_environment_cells() {
    let build = compile(
        "import { value } from 'host'; function run() { return value; }",
        &CompileOptions {
            imports: manifest(&[("host", &["value"])]),
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
        &CompileOptions {
            imports: manifest(&[("host", &["value"])]),
        },
    )
    .expect("readonly import assignment lowers");
    let script = build
        .functions
        .iter()
        .find(|function| function.func_id == build.script)
        .unwrap();
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
        &CompileOptions::default(),
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
        &CompileOptions::default(),
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
        compile(&format!("{literal};"), &CompileOptions::default()).unwrap();
    }
    let error = compile("\n  /(/;", &CompileOptions::default()).unwrap_err();
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
        let error = compile(&source, &CompileOptions::default()).unwrap_err();
        assert!(error.message().contains("before initialization"), "{error}");
        assert_eq!((error.line(), error.column()), (2, 3), "{error}");
    }
}

#[test]
fn names_nested_in_exported_patterns_are_public() {
    let build = compile(
        "export const { x: [a], ...rest } = { x: [1] };",
        &CompileOptions::default(),
    )
    .unwrap();
    assert_eq!(
        build.exports.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a", "rest"]
    );
}

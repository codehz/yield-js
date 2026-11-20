use yield_compiler::{CompileOptions, compile};
use yield_vm::bytecode::Instr;

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
    let build = compile(
        source,
        &CompileOptions {
            imports: vec!["inspect".into(), "seed".into(), "wait".into()],
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
    let error = compile("function run() { class A {} }", &CompileOptions::default())
        .expect_err("class is outside the PoC subset");
    assert!(error.message().contains("unsupported syntax"));
    assert_eq!(error.line(), 1);
    assert!(error.column() > 1);
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
    imports: &'static [&'static str],
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
            imports: &["seed"],
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
            imports: &["inspect"],
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
                imports: case.imports.iter().map(|name| (*name).to_owned()).collect(),
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
            vec!["inspect".to_owned()]
        } else {
            Vec::new()
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
        ("\nexport { inspect };", "inspect"),
    ] {
        let options = CompileOptions {
            imports: vec!["inspect".into()],
        };
        let error = compile(source, &options).unwrap_err();
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
        ("import 'dep';", "import declaration"),
        ("import('dep');", "dynamic import"),
        ("import.meta;", "import.meta"),
        ("export default 1;", "default export"),
        ("let x; export { x as default };", "default export"),
        ("export { x } from 'dep';", "re-export"),
        ("export * from 'dep';", "re-export"),
        ("export * as ns from 'dep';", "re-export"),
        ("export class C {}", "class declaration"),
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

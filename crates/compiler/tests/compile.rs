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
fn rejects_delete_of_property_with_location() {
    let error = compile(
        "function run(o) { return delete ((o.x)); }",
        &CompileOptions::default(),
    )
    .expect_err("property delete is outside the PoC subset");
    assert!(
        error.message().contains("unsupported syntax: delete"),
        "{error}"
    );
    assert_eq!(error.line(), 1);
    assert!(error.column() > 1);
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
fn rejects_unsupported_object_member_syntax_with_location() {
    for (member, message) in [
        ("...other", "object spread"),
        ("*m() {}", "generator"),
        ("m() { return super.x; }", "unsupported syntax"),
        ("set x({a}) {}", "parameter destructuring"),
        ("set x(v = 1) {}", "default parameter"),
    ] {
        let source = format!("function run() {{\n  return {{ {member} }};\n}}");
        let error = compile(&source, &CompileOptions::default()).expect_err(member);
        assert!(error.message().contains(message), "{member}: {error}");
        assert_eq!(error.line(), 2);
        assert!(error.column() > 2);
    }
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
        let (object, key, src) = corrupt.functions[0]
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

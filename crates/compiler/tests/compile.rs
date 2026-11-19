use yield_compiler::{CompileOptions, compile};
use yield_vm::bytecode::{Instr, ParamTarget};

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

#[test]
fn compiles_design_example_with_two_resume_points() {
    let build = compile(
        RUN,
        &CompileOptions {
            imports: vec!["inspect".into(), "seed".into(), "wait".into()],
        },
    )
    .expect("source compiles");
    assert_eq!(build.functions.len(), 1);
    let function = &build.functions[0];
    assert_eq!(function.name, "run");
    assert!(function.is_async);
    assert_eq!(function.resumes.len(), 2);
    let await_count = function
        .code
        .iter()
        .filter(|instruction| matches!(instruction, Instr::Await { .. }))
        .count();
    assert_eq!(await_count, 2);
    assert_eq!(function.resumes[0].saved_slots, vec![0]);
    assert_eq!(function.resumes[1].saved_slots, vec![1]);
    assert!(matches!(function.params.as_slice(), [ParamTarget::Slot(_)]));
    assert!(function.handlers.is_empty());
    eprintln!("{function:#?}");
}

#[test]
fn rejects_unsupported_syntax_with_location() {
    let error = compile("function run() { class A {} }", &CompileOptions::default())
        .expect_err("class is outside the PoC subset");
    assert!(error.message().contains("unsupported syntax"));
    assert_eq!(error.line(), 1);
    assert!(error.column() > 1);
}

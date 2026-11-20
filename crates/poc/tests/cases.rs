include!(concat!(env!("OUT_DIR"), "/cases.rs"));

#[test]
fn private_run_is_not_a_case_entrypoint() {
    let source = "// @returns 1\n\nfunction run() { return 1; }";
    let error = yield_poc::run_case("private_run.js", source).unwrap_err();
    assert!(error.contains("unknown export \"run\""), "{error}");

    let mut host = yield_poc::TestHost::default();
    let vm = yield_vm::Vm::new(yield_poc::build(source), &mut host).unwrap();
    assert!(
        matches!(vm.get_export("run"), Err(yield_vm::VmError::InvalidState(message)) if message == "unknown export \"run\"")
    );
}

#[test]
fn case_matcher_failure_reports_arguments_after_restore() {
    let source = include_str!("failing_cases/matcher_failure.js");
    let error = yield_poc::run_case("matcher_failure.js", source).unwrap_err();
    for detail in ["toBe", "3", "4"] {
        assert!(error.contains(detail), "{error}");
    }
}

#[test]
fn case_assertion_failure_reports_checkpoint() {
    let source = r#"// @returns undefined
import { assert } from 'test';
export function run() { assert(false, 'checkpoint'); }
"#;
    let error = yield_poc::run_case("assertion_failure.js", source).unwrap_err();
    assert!(error.contains("checkpoint"), "{error}");
}

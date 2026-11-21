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
fn case_matcher_to_throw_reports_pattern_and_message() {
    let source = r#"// @returns undefined
import { expect } from 'test';
export function run() {
    expect(() => { throw new Error('out of stock'); }).toThrow(/available/i);
}
"#;
    let error = yield_poc::run_case("matcher_to_throw_failure.js", source).unwrap_err();
    for detail in ["toThrow", "/available/i", "out of stock"] {
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

#[test]
fn case_advance_rejects_invalid_arguments_with_location() {
    for (argument, diagnostic) in [
        ("", "missing argument"),
        ("-1", "expected a non-negative decimal integer"),
        ("+1", "expected a non-negative decimal integer"),
        ("1.5", "expected a non-negative decimal integer"),
        ("1 2", "expected a non-negative decimal integer"),
        ("18446744073709551616", "value exceeds u64::MAX"),
    ] {
        let source = format!(
            "\n// @advance {argument}\n// @returns 1\nexport function run() {{ return 1; }}"
        );
        let error = yield_poc::run_case("invalid_advance.js", &source).unwrap_err();
        assert_eq!(
            error,
            format!("invalid_advance.js:2: @advance: {diagnostic}")
        );
    }
}

#[test]
fn case_advance_reports_clock_overflow_with_location() {
    let source = r#"// @advance 18446744073709551615
// @advance 1
// @returns 1
export function run() { return 1; }
"#;
    let error = yield_poc::run_case("advance_overflow.js", source).unwrap_err();
    assert!(
        error.starts_with("advance_overflow.js:2: @advance: "),
        "{error}"
    );
    assert!(error.contains("overflow"), "{error}");
}

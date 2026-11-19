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

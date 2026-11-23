use std::sync::{Arc, Mutex};

use yield_harness::{TestHost, build};
use yield_vm::binding::{HostFault, JsValue};
use yield_vm::{CryptoService, SnapshotValidation, Value, Vm};

const CHECK_ERRORS: &str = r#"
const saved = crypto;
export function run(expected) {
    const bytes = new Uint8Array([7, 8]);
    const calls = [() => crypto.getRandomValues(bytes), () => crypto.randomUUID()];
    for (const call of calls) {
        let caught;
        try { call(); } catch (e) { caught = e; }
        if (!(caught instanceof DOMException) || caught.name !== expected) throw caught;
    }
    return saved === crypto && crypto === globalThis.crypto && bytes[0] === 7 && bytes[1] === 8;
}
"#;

#[test]
fn crypto_without_entropy_fails_lazily_after_init_and_restore() {
    let build = build(CHECK_ERRORS);
    let mut missing = TestHost::without_crypto_service();
    let mut vm = Vm::new(build.clone(), &mut missing, Default::default()).unwrap();
    assert_eq!(
        vm.start(
            &mut missing,
            "run",
            vec![JsValue::string("NotSupportedError")],
        )
        .unwrap(),
        Value::boolean(true)
    );

    let mut enabled = TestHost::new();
    let vm = Vm::new(build.clone(), &mut enabled, Default::default()).unwrap();
    let mut vm = Vm::restore(
        vm.snapshot(&[]).unwrap(),
        build,
        &mut missing,
        yield_vm::RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        vm.start(
            &mut missing,
            "run",
            vec![JsValue::string("NotSupportedError")],
        )
        .unwrap(),
        Value::boolean(true)
    );
}

#[test]
fn crypto_faults_are_catchable_without_partial_mutation() {
    #[derive(Default)]
    struct FaultyEntropy(Mutex<Vec<usize>>);

    impl CryptoService for FaultyEntropy {
        fn fill_random(&self, bytes: &mut [u8]) -> Result<(), HostFault> {
            self.0.lock().unwrap().push(bytes.len());
            bytes[0] = 42;
            Err(HostFault::new("entropy unavailable"))
        }
    }

    let service = Arc::new(FaultyEntropy::default());
    let mut host = TestHost::with_crypto_service(service.clone());
    let mut vm = Vm::new(build(CHECK_ERRORS), &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![JsValue::string("OperationError")])
            .unwrap(),
        Value::boolean(true)
    );
    assert_eq!(*service.0.lock().unwrap(), [2, 16]);
}

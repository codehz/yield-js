use std::sync::{Arc, Mutex};

use yield_harness::{TestHost, build};
use yield_vm::binding::{HostFault, JsValue};
use yield_vm::{
    CryptoService, PromiseState, RestoreOptions, Snapshot, SnapshotValidation, Value, Vm, VmError,
};

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
    let missing = TestHost::without_crypto_service();
    let mut vm = Vm::new(build.clone(), missing.host(), Default::default()).unwrap();
    assert_eq!(
        vm.start("run", vec![JsValue::string("NotSupportedError")],)
            .unwrap(),
        Value::boolean(true)
    );

    let enabled = TestHost::new();
    let vm = Vm::new(build.clone(), enabled.host(), Default::default()).unwrap();
    let mut vm = Vm::restore(
        vm.snapshot(&[]).unwrap(),
        build,
        missing.host(),
        yield_vm::RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        vm.start("run", vec![JsValue::string("NotSupportedError")],)
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
    let host = TestHost::with_crypto_service(service.clone());
    let mut vm = Vm::new(build(CHECK_ERRORS), host.host(), Default::default()).unwrap();
    assert_eq!(
        vm.start("run", vec![JsValue::string("OperationError")])
            .unwrap(),
        Value::boolean(true)
    );
    assert_eq!(*service.0.lock().unwrap(), [2, 16]);
}

#[test]
fn subtle_without_service_rejects_without_disabling_entropy() {
    let source = r#"
export async function run() {
    const api = crypto.subtle;
    const bytes = new Uint8Array(4);
    crypto.getRandomValues(bytes);
    if (bytes.toHex() !== "00010203" || !api || api !== crypto.subtle) return false;
    for (const call of [
        () => api.digest("SHA-256", bytes),
        () => api.importKey("raw", new Uint8Array(16), "AES-GCM", true, ["encrypt"]),
    ]) {
        const result = call();
        if (!(result instanceof Promise)) return false;
        try { await result; } catch (error) {
            if (error instanceof DOMException && error.name === "NotSupportedError") continue;
            throw error;
        }
        return false;
    }
    return true;
}
"#;
    let mut host = TestHost::without_subtle_service();
    let mut vm = Vm::new(build(source), host.host(), Default::default()).unwrap();
    let result = vm.start("run", vec![]).unwrap();
    assert_eq!(vm.promise_state(&result).unwrap(), PromiseState::Pending);
    host.drain_subtle(&mut vm).unwrap();
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::boolean(true))
    );
}

#[test]
fn subtle_key_and_ciphertext_survive_snapshot_with_identity() {
    let source = r#"
const subtle = crypto.subtle;
const params = { name: "AES-GCM", iv: new Uint8Array(12), additionalData: new Uint8Array([4]) };
let key, saved, sealed;
export async function prepare() {
    key = await subtle.importKey("raw", new Uint8Array(16), "AES-GCM", true, ["encrypt", "decrypt"]);
    saved = new Map([[key, key]]);
    sealed = await subtle.encrypt(params, key, new Uint8Array([10, 20, 30]));
    return true;
}
export async function open() {
    if (subtle !== crypto.subtle || saved.get(key) !== key || !(key instanceof CryptoKey) ||
        key.algorithm.name !== "AES-GCM" || key.usages.join(",") !== "encrypt,decrypt") return false;
    return new Uint8Array(await subtle.decrypt(params, key, sealed)).toHex() === "0a141e";
}
"#;
    let build = build(source);
    let mut host = TestHost::new();
    let mut vm = Vm::new(build.clone(), host.host(), Default::default()).unwrap();
    let prepared = vm.start("prepare", vec![]).unwrap();
    host.drain_subtle(&mut vm).unwrap();
    assert_eq!(
        vm.promise_state(&prepared).unwrap(),
        PromiseState::Fulfilled(Value::boolean(true))
    );
    let image = vm.snapshot(&[]).unwrap().to_bytes().unwrap();
    drop(vm);
    let mut restored_host = TestHost::new();
    let mut restored = Vm::restore(
        Snapshot::from_bytes(&image).unwrap(),
        build,
        restored_host.host(),
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    let opened = restored.start("open", vec![]).unwrap();
    restored_host.drain_subtle(&mut restored).unwrap();
    assert_eq!(
        restored.promise_state(&opened).unwrap(),
        PromiseState::Fulfilled(Value::boolean(true))
    );
}

#[test]
fn subtle_pending_worker_blocks_snapshot_even_without_guest_roots() {
    struct NonCloneDiagnostics;
    impl yield_vm::Diagnostics for NonCloneDiagnostics {
        fn report(&self, _: yield_vm::Diagnostic<'_>) {}
    }

    use yield_vm::PlatformServices as _;
    let mut host = TestHost::new();
    let subtle_crypto = host.host().platform().subtle_crypto().cloned().unwrap();
    let platform_host = yield_vm::host! {
        services: {
            diagnostics: NonCloneDiagnostics,
            subtle_crypto: subtle_crypto,
        },
    }
    .unwrap();
    let mut vm = Vm::new(
        super::support::compile_main(
            r#"
export function run() {
    return crypto.subtle.digest("SHA-256", new Uint8Array([97, 98, 99]));
}
"#,
            &yield_compiler::Compiler::default(),
        )
        .unwrap(),
        platform_host,
        Default::default(),
    )
    .unwrap();
    let promise = vm.start("run", vec![]).unwrap();
    // Completion is applied only when the VM drains the worker result, so the snapshot
    // check does not race the worker even if the digest has already been computed.
    assert_eq!(vm.promise_state(&promise).unwrap(), PromiseState::Pending);
    assert!(matches!(
        vm.snapshot(&[]),
        Err(VmError::InvalidState(message)) if message.contains("pending SubtleCrypto operations")
    ));
    host.drain_subtle(&mut vm).unwrap();
    assert!(matches!(
        vm.promise_state(&promise).unwrap(),
        PromiseState::Fulfilled(_)
    ));
    assert!(vm.snapshot(&[]).unwrap().operations.is_empty());
}

#[test]
fn subtle_local_import_is_pending_before_the_microtask_checkpoint() {
    let mut host = TestHost::new();
    let mut vm = Vm::new(
        build(
            r#"
export function run() {
    return crypto.subtle.importKey("raw", new Uint8Array(16), "AES-GCM", true, ["encrypt"]);
}
"#,
        ),
        host.host(),
        Default::default(),
    )
    .unwrap();
    let promise = vm.start("run", vec![]).unwrap();
    assert_eq!(vm.promise_state(&promise).unwrap(), PromiseState::Pending);
    assert!(!vm.has_pending_subtle_operations());
    host.drain_subtle(&mut vm).unwrap();
    assert!(matches!(
        vm.promise_state(&promise).unwrap(),
        PromiseState::Fulfilled(_)
    ));
}

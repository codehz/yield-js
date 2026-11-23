use yield_harness::{TestHost, build};
use yield_vm::{Object, RestoreOptions, Snapshot, SnapshotValidation, Vm, VmError};

fn full() -> RestoreOptions {
    RestoreOptions {
        validation: SnapshotValidation::Full,
        ..Default::default()
    }
}

#[test]
fn restore_preserves_fetch_environment_and_rejects_wider_authorization() {
    use yield_vm::{FetchEnvironment, Origin, VmOptions};

    let environment = FetchEnvironment {
        origin: Some(Origin {
            scheme: "https".into(),
            host: "example.test".into(),
            port: 443,
        }),
        referrer: Some("https://example.test/page".into()),
        partition: "isolated".into(),
        authorization: "loopback-only".into(),
    };
    let build = build("export function run() { return 1; }");
    let mut host = TestHost::default();
    let vm = Vm::new(
        build.clone(),
        &mut host,
        VmOptions {
            fetch_environment: Some(environment.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = vm.snapshot(&[]).unwrap();
    assert_eq!(snapshot.fetch_environment, environment);
    assert!(matches!(
        Vm::restore(
            snapshot.clone(),
            build.clone(),
            &mut host,
            RestoreOptions::default()
        ),
        Err(VmError::InvalidSnapshot(message)) if message.contains("not authorized")
    ));
    let widened = FetchEnvironment {
        authorization: "all-network".into(),
        ..environment.clone()
    };
    assert!(matches!(
        Vm::restore(
            snapshot.clone(),
            build.clone(),
            &mut host,
            RestoreOptions {
                fetch_environment: Some(widened),
                ..Default::default()
            }
        ),
        Err(VmError::InvalidSnapshot(message)) if message.contains("not authorized")
    ));
    let mut restored = Vm::restore(
        snapshot,
        build,
        &mut host,
        RestoreOptions {
            fetch_environment: Some(environment),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        restored.start(&mut host, "run", vec![]).unwrap().view(),
        yield_vm::ValueRef::Number(number) if number == 1.0
    ));
}

#[test]
fn fetch_offline_objects_survive_gc_and_binary_restore() {
    let build = build(
        r#"
        const url = URL.createObjectURL(new Blob(["unbound"]));
        const form = new FormData();
        form.append("x", new File(["abc"], "one", {lastModified: 4}));
        form.append("x", "two");
        const cursor = form.values();
        cursor.next();
        export function run() {
            return form.get("x").name + form.get("x").lastModified + cursor.next().value;
        }
        export function revoke() { URL.revokeObjectURL(url); }
    "#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    vm.collect_garbage(&[]).unwrap();
    let snapshot = Snapshot::from_bytes(&vm.snapshot(&[]).unwrap().to_bytes().unwrap()).unwrap();
    assert!(
        snapshot
            .objects
            .iter()
            .any(|r| matches!(r.object, Object::Blob { .. }))
    );
    let mut corrupted = snapshot.clone();
    let form = corrupted
        .objects
        .iter_mut()
        .find(|r| matches!(r.object, Object::File { .. }))
        .unwrap();
    let mut record = serde_json::to_value(&form.object).unwrap();
    record["file"]["data"]["blob"]["start"] = serde_json::json!(1000);
    form.object = serde_json::from_value(record).unwrap();
    assert!(
        matches!(Vm::restore(corrupted, build.clone(), &mut host, full()), Err(VmError::InvalidSnapshot(message)) if message.contains("invalid File"))
    );
    let mut restored = Vm::restore(snapshot, build, &mut host, full()).unwrap();
    let result = restored.start(&mut host, "run", vec![]).unwrap();
    assert_eq!(result.into_string().unwrap().to_utf8().unwrap(), "one4two");
    restored.start(&mut host, "revoke", vec![]).unwrap();
    restored.collect_garbage(&[]).unwrap();
    assert!(
        !restored
            .snapshot(&[])
            .unwrap()
            .objects
            .iter()
            .any(|r| matches!(r.object, Object::Blob { .. }))
    );
}

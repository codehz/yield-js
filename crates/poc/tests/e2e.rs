#[path = "e2e/mod.rs"]
mod suite;

#[test]
fn restore_rejects_a_missing_boxed_symbol_payload() {
    use yield_poc::{TestHost, build};
    use yield_vm::heap::BoxedPrimitive;
    use yield_vm::{Object, Vm, VmError};

    let build = build(
        r#"
export async function run() {
  const boxed = Object(Symbol("boxed-only"));
  await wait("a");
  return boxed;
}
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start run");
    vm.run_until_idle(&mut host).expect("suspend run");
    let mut snapshot = vm.snapshot(&[result]).expect("capture boxed symbol");
    let symbol = snapshot
        .objects
        .iter()
        .find_map(|record| match &record.object {
            Object::Boxed {
                primitive: BoxedPrimitive::Symbol(id),
                ..
            } => Some(*id),
            _ => None,
        })
        .expect("boxed symbol object");
    assert_eq!(
        snapshot.symbols.remove(&symbol),
        Some(Some("boxed-only".into()))
    );
    assert!(matches!(
        Vm::restore(snapshot, build, &mut host),
        Err(VmError::InvalidSnapshot(message)) if message.contains("missing symbol")
    ));
}

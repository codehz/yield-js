//! Global aliases and runtime identification survive restore without re-reading
//! host configuration.

use yield_harness::{TestHost, build};
use yield_vm::{Vm, VmOptions};

fn read(vm: &mut Vm) -> String {
    vm.start("read", vec![])
        .expect("start read")
        .into_string()
        .expect("read returns a string")
        .to_utf8()
        .expect("read returns UTF-8")
}

#[test]
fn navigator_identifies_the_runtime_and_survives_restore() {
    let build = build(
        r#"
const saved = navigator;
export function read() {
  return [
    self === globalThis,
    saved === navigator,
    navigator === globalThis.navigator,
    navigator.userAgent,
    navigator.platform,
  ].join('|');
}
"#,
    );
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);

    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), host.host(), VmOptions::default()).expect("create VM");
    let default = format!("true|true|true|yield-js|{platform}");
    assert_eq!(read(&mut vm), default);

    let snapshot = vm.snapshot(&[]).expect("snapshot");
    host.set_user_agent("changed/9");
    let mut restored =
        Vm::restore(snapshot, build.clone(), host.host(), Default::default()).expect("restore");
    assert_eq!(
        read(&mut restored),
        default,
        "restore keeps the recorded navigator instead of the host's new value"
    );

    let mut host = TestHost::default();
    host.set_user_agent("test-runtime/1.2");
    let mut vm = Vm::new(build, host.host(), VmOptions::default()).expect("create VM");
    assert_eq!(
        read(&mut vm),
        format!("true|true|true|test-runtime/1.2|{platform}")
    );
}

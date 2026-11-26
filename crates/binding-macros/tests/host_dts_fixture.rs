//! A real consumer check: `host!` reads the declaration file while
//! rustc expands the macro, so a missing declared value is a compile error even
//! when no script imports it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    // Cargo runs integration tests from the package root. Do not bake in a
    // checkout path: a shared target directory can reuse this test binary.
    Path::new("../..").canonicalize().expect("workspace root")
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("yield-{name}-fixture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).expect("fixture source dir");
        let vm = workspace_root().join("crates/vm");
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"yield-fixture-{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
                 [workspace]\n\n[dependencies]\nyield-vm = {{ path = \"{}\" }}\n\n\
                 [patch.crates-io]\nbrotli-decompressor = {{ path = \"{}\" }}\n",
                vm.display(),
                vm.join("src/builtin/streams/compression/vendor/brotli-decompressor").display()
            ),
        )
        .expect("fixture manifest");
        Self { dir }
    }

    fn library(&self, required_kind: &str, extra: &str) -> String {
        let constant = if required_kind == "constant" {
            "#[constant(name = \"required\")]\n        const REQUIRED: f64 = 2.0;"
        } else {
            ""
        };
        format!(
            r#"use yield_vm::{{Host, JsResult, JsValue, ModuleCx, host, host_module, module_exports}};

#[host_module(name = "a")]
struct A;

#[module_exports]
impl A {{
    #[function(name = "used")]
    fn used(_cx: &mut ModuleCx<'_, Self>) -> JsResult<JsValue> {{
        Ok(JsValue::number(1.0))
    }}
}}

#[host_module(name = "b")]
struct B;

#[module_exports]
impl B {{
    {constant}

    #[constant(name = "privateExtra")]
    const PRIVATE_EXTRA: f64 = 1.0;
    {extra}
}}

pub fn make_host() -> Result<Host, String> {{
    host! {{
        dts: "host.d.ts",
        modules: [A, B],
        services: {{}},
    }}
}}
"#
        )
    }

    fn write(&self, path: &str, text: &str) {
        fs::write(self.dir.join(path), text).expect(path);
    }

    fn check(&self) -> (bool, String) {
        let output = Command::new(env!("CARGO"))
            .args(["check", "--offline", "--manifest-path"])
            .arg(self.dir.join("Cargo.toml"))
            .env(
                "CARGO_TARGET_DIR",
                workspace_root().join("target/host-dts-fixture"),
            )
            .env("CARGO_TERM_COLOR", "never")
            .output()
            .expect("cargo check runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn declared_exports_are_checked_while_expanding_the_registry() {
    let fixture = Fixture::new("host-dts");
    fixture.write(
        "src/host.d.ts",
        r#"declare module "a" {
    export function used(): unknown;
}

declare module "b" {
    export function required(): void;
}
"#,
    );
    fixture.write("src/lib.rs", &fixture.library("", ""));

    let (ok, output) = fixture.check();
    assert!(!ok, "missing declared export must fail:\n{output}");
    assert!(
        output.contains("missing host export: b:required"),
        "{output}"
    );

    fixture.write("src/lib.rs", &fixture.library("constant", ""));
    let (ok, output) = fixture.check();
    assert!(
        ok,
        "a same-name constant implementation and extra names must pass:\n{output}"
    );

    fixture.write(
        "src/host.d.ts",
        r#"declare module "a" {
    export function used(): unknown;
}

declare module "b" {
    export function required(): void;
    export function added(): void;
}
"#,
    );
    let (ok, output) = fixture.check();
    assert!(!ok, "a newly declared export must fail:\n{output}");
    assert!(output.contains("missing host export: b:added"), "{output}");
}

#[test]
fn service_only_hosts_need_no_dts_but_still_check_supplied_declarations() {
    let fixture = Fixture::new("service-only-host");
    let source = r#"
pub fn make_host() -> Result<yield_vm::Host, String> {
    yield_vm::host! { services: { u64 => 17 } }
}
"#;
    fixture.write("src/lib.rs", source);
    let (ok, output) = fixture.check();
    assert!(
        ok,
        "a service-only host must compile without a DTS file:\n{output}"
    );

    fixture.write(
        "src/host.d.ts",
        "declare module 'example' { export const missing: number; }",
    );
    fixture.write(
        "src/lib.rs",
        &source.replace("services:", "dts: \"host.d.ts\", services:"),
    );
    let (ok, output) = fixture.check();
    assert!(
        !ok && output.contains("missing host export: example:missing"),
        "{output}"
    );
}

#[test]
fn typed_modules_reject_foreign_contexts_and_legacy_states() {
    let declarations = "declare module 'counter' { export function bump(): void; }";
    let source = r#"
use yield_vm::{Host, JsResult, ModuleCx, host, host_module, module_exports};

#[host_module(name = "counter")]
struct Counter;

#[host_module(name = "other")]
struct Other;

#[module_exports]
impl Counter {
    #[function(name = "bump")]
    fn bump(_: &mut ModuleCx<'_, Self>) -> JsResult<()> { Ok(()) }
}

pub fn make_host() -> Result<Host, String> {
    host! { dts: "host.d.ts", modules: [Counter], services: {} }
}
"#;
    let fixture = Fixture::new("typed-modules");
    fixture.write("src/host.d.ts", declarations);
    fixture.write("src/lib.rs", source);
    let (ok, output) = fixture.check();
    assert!(ok, "baseline typed module must compile:\n{output}");

    fixture.write("src/lib.rs", &source.replace("dts: \"host.d.ts\", ", ""));
    let (ok, output) = fixture.check();
    assert!(!ok && output.contains("missing `dts`"), "{output}");

    let fixture = Fixture::new("foreign-module-context");
    fixture.write("src/host.d.ts", declarations);
    fixture.write(
        "src/lib.rs",
        &source.replace("ModuleCx<'_, Self>", "ModuleCx<'_, Other>"),
    );
    let (ok, output) = fixture.check();
    assert!(
        !ok && output.contains("first parameter must be &mut ModuleCx<'_, Self>"),
        "{output}"
    );

    let fixture = Fixture::new("legacy-states-section");
    fixture.write("src/host.d.ts", declarations);
    fixture.write(
        "src/lib.rs",
        &source.replace("modules: [Counter],", "modules: [Counter], states: [],"),
    );
    let (ok, output) = fixture.check();
    assert!(!ok && output.contains("unknown host section"), "{output}");
}

#[test]
fn explicit_service_registration_rejects_the_wrong_value_type() {
    let fixture = Fixture::new("service-type");
    fixture.write(
        "src/lib.rs",
        r#"use std::sync::Arc;
use yield_vm::{Host, host};

trait CustomService {}

pub fn make_host() -> Result<Host, String> {
    host! { services: { Arc<dyn CustomService> => 17u64 } }
}
"#,
    );
    let (ok, output) = fixture.check();
    assert!(
        !ok,
        "a value of the wrong service type must fail:\n{output}"
    );
    assert!(
        output.contains("E0308") && output.contains("CustomService") && output.contains("u64"),
        "{output}"
    );
}

#[test]
fn service_storage_cannot_be_moved_to_another_thread() {
    let fixture = Fixture::new("thread-affine-services");
    fixture.write(
        "src/lib.rs",
        r#"use yield_vm::Host;

fn require_send<T: Send>() {}

pub fn move_services() {
    require_send::<Host>();
}
"#,
    );
    let (ok, output) = fixture.check();
    assert!(
        !ok && output.contains("src/lib.rs:6:") && output.contains("E0277"),
        "{output}"
    );
}

#[test]
fn selected_consumers_require_static_service_providers() {
    let fixture = Fixture::new("static-services");
    fixture.write("src/host.d.ts", "declare module 'test' {}");
    let source = r#"
use yield_vm::{Host, host};

mod bindings {
    use yield_vm::{Decode, Encode, HostFault, OperationDriver, OperationToken, Registration,
        Services, host_class, host_module, module_exports, operation};

    #[host_class(tag = "NeedsClass", services = [value: u32])]
    struct Class;

    #[operation(name = "needs-op", recovery = pause, services = [value: u64])]
    #[derive(Encode, Decode)]
    struct Op;

    impl Registration<Op> for () {}

    impl OperationDriver for Op {
        type Live = ();
        fn start(&self, _: OperationToken, _: &Services<'_, Self>) -> Result<(), HostFault> {
            Ok(())
        }
    }

    #[host_module(name = "test", classes = [Class], operations = [Op], services = [value: u16])]
    pub struct Module;

    #[module_exports]
    impl Module {}
}

pub fn make_host() -> Result<Host, String> {
    host! {
        dts: "host.d.ts",
        modules: [bindings::Module],
        services: { u16 => 1, u32 => 2, u64 => 3, },
    }
}

pub fn no_consumers() -> Result<Host, String> {
    type Alias = u8;
    host! { services: { Alias => 4, } }
}
"#;
    fixture.write("src/lib.rs", source);
    let (ok, output) = fixture.check();
    assert!(ok, "baseline service registrations must compile:\n{output}");

    for (entry, ty) in [
        ("u16 => 1,", "u16"),
        ("u32 => 2,", "u32"),
        ("u64 => 3,", "u64"),
    ] {
        fixture.write("src/lib.rs", &source.replace(entry, ""));
        let (ok, output) = fixture.check();
        assert!(
            !ok && output.contains("Provide") && output.contains(ty),
            "missing {ty}:\n{output}"
        );
    }

    fixture.write(
        "src/lib.rs",
        &source.replace("Alias => 4,", "Alias => 4, u8 => 5,"),
    );
    let (ok, output) = fixture.check();
    assert!(
        !ok && output.contains("E0283")
            && output.contains("Provide")
            && output.contains("assert_unique"),
        "duplicate types must be ambiguous even without consumers:\n{output}"
    );
}

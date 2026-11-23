//! A real consumer check: `host_bindings!` reads the declaration file while
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
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("yield-host-dts-fixture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).expect("fixture source dir");
        let vm = workspace_root().join("crates/vm");
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"host-dts-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
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
            r#"use yield_vm::binding::CallCx;
use yield_vm::{{JsResult, JsValue, host_bindings, host_module}};

#[host_module(name = "a")]
mod a {{
    use super::*;

    #[function(name = "used")]
    fn used(_cx: &mut CallCx<'_>) -> JsResult<JsValue> {{
        Ok(JsValue::number(1.0))
    }}
}}

#[host_module(name = "b")]
mod b {{
    use super::*;

    {constant}

    #[constant(name = "privateExtra")]
    const PRIVATE_EXTRA: f64 = 1.0;
    {extra}
}}

pub fn bindings() -> Result<yield_vm::Bindings, String> {{
    host_bindings! {{
        dts: "host.d.ts",
        modules: [a, b],
        classes: [],
        states: [],
        operations: [],
        services: [],
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
            .args([
                "check",
                "--offline",
                "--message-format",
                "short",
                "--manifest-path",
            ])
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
    let fixture = Fixture::new();
    fixture.write(
        "host.d.ts",
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
        "host.d.ts",
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

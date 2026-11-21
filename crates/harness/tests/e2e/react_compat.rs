//! Compatibility probe: real React 19 source compiled by the strict-ESM subset.
//!
//! The vendored bundle is expected NOT to compile today. The test pins the
//! current diagnostic so any change in support (a different message, a
//! different location, or a successful compile) turns the test red and forces
//! a docs refresh. See the compatibility notes in `docs/gaps.md`.

use yield_compiler::{CompileError, CompileOptions};

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");

#[test]
fn real_react_19_bundle_fails_to_compile_with_a_pinned_diagnostic() {
    let error: CompileError = match yield_compiler::compile(
        REACT_19_BUNDLE,
        &CompileOptions {
            imports: Default::default(),
        },
    ) {
        Err(error) => error,
        Ok(_) => panic!(
            "React 19 (esm.sh) now compiles. Turn e2e/react_compat.rs into real \
             execution cases and refresh docs/gaps.md / docs/implemented.md."
        ),
    };
    assert_eq!(
        (error.message(), error.line(), error.column()),
        ("unsupported syntax: default export", 7, 9121),
        "React 19 compile diagnostic changed; refresh the pinned expectation in \
         e2e/react_compat.rs and the compatibility notes in docs/gaps.md"
    );
}

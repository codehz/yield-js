//! Compatibility probe: real React 19 source compiled by the strict-ESM subset.
//!
//! The bundle uses browser global `reportError`; provide it through a preload
//! module to expose the next independent compiler limitation.

use yield_compiler::{CompileError, CompileOptions};

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");
const PRELOAD: &str = "export function reportError(error) {}";

#[test]
fn real_react_19_bundle_reports_next_pinned_diagnostic() {
    let error: CompileError = match yield_compiler::compile_with_preload(
        REACT_19_BUNDLE,
        PRELOAD,
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
        ("unknown binding \"window\"", 7, 4315),
        "React 19 compile diagnostic changed; refresh docs/gaps.md"
    );
}

#[test]
fn minimal_input_confirms_the_unknown_global_cause() {
    let error: CompileError = yield_compiler::compile(
        "function probe(){ reportError('x'); }",
        &CompileOptions {
            imports: Default::default(),
        },
    )
    .unwrap_err();
    assert_eq!(error.message(), "unknown callee \"reportError\"", "{error}");
}

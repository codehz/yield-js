//! Compatibility probe: real React 19 source compiled by the strict-ESM subset.
//!
//! The default-alias form in the bundle (`export { Je as default, … }`)
//! compiles now. The bundle next fails on an independent gap: it reads the
//! browser global `reportError`, which is neither a local binding, a host
//! import, nor a builtin, so name resolution rejects it at compile time. The
//! test pins that diagnostic so any change in support forces a docs refresh.

use yield_compiler::{CompileError, CompileOptions};

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");

#[test]
fn real_react_19_bundle_fails_with_a_pinned_diagnostic() {
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
    // The default-alias statements no longer stop compilation: the pinned
    // location is `reportError`, far past the export list at line 7.
    assert_eq!(
        (error.message(), error.line(), error.column()),
        ("unknown binding \"reportError\"", 7, 4256),
        "React 19 compile diagnostic changed; refresh the pinned expectation in \
         e2e/react_compat.rs and the compatibility notes in docs/gaps.md"
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

//! Compatibility probe: real React 19 source compiled by the strict-ESM subset.
//!
//! Environment globals absent from the VM are supplied by the preload module.
//! This probes compilation only, not runtime behavior.
use std::collections::{BTreeMap, BTreeSet};

use yield_compiler::{CompileError, CompileOptions};

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");
const PRELOAD: &str = "import * as console from 'console'; export { console }; export function reportError(error) {} export const window = undefined; export const process = undefined;";

#[test]
fn real_react_19_bundle_compiles_with_environment_polyfills() {
    yield_compiler::compile_with_preload(
        REACT_19_BUNDLE,
        PRELOAD,
        &CompileOptions {
            imports: BTreeMap::from([(
                "console".to_owned(),
                BTreeSet::from(["debug", "error", "info", "log", "warn"].map(str::to_owned)),
            )]),
        },
    )
    .unwrap_or_else(|error| panic!("React 19 compile diagnostic changed: {error}"));
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

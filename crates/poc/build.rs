use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=tests/cases");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let cases_dir = Path::new(&manifest_dir).join("tests/cases");
    if !cases_dir.is_dir() {
        panic!("tests/cases directory is missing");
    }

    let entries = fs::read_dir(&cases_dir)
        .unwrap_or_else(|error| panic!("failed to read tests/cases: {error}"));
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("failed to read tests/cases: {error}"));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("failed to stat {}: {error}", path.display()));
        if !file_type.is_file() {
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("js") {
            continue;
        }
        files.push(path);
    }
    files.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    if files.is_empty() {
        panic!("tests/cases contains no .js cases");
    }

    let mut generated = String::new();
    for path in &files {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_else(|| panic!("invalid case file name {}", path.display()));
        if !valid_stem(stem) {
            panic!("invalid case file stem `{stem}` ({})", path.display());
        }
        let path_literal = format!(
            "{:?}",
            path.to_str()
                .unwrap_or_else(|| panic!("case path is not UTF-8: {}", path.display()))
        );
        let function = rust_fn_name(stem);
        generated.push_str(&format!(
            "#[test]\nfn {function}() {{\n    let source = include_str!({path_literal});\n    if let Err(error) = yield_poc::run_case(\"{stem}.js\", source) {{\n        panic!(\"{{error}}\");\n    }}\n}}\n\n"
        ));
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let destination = Path::new(&out_dir).join("cases.rs");
    fs::write(&destination, generated)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", destination.display()));
}

fn valid_stem(stem: &str) -> bool {
    let mut chars = stem.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|char| char.is_ascii_lowercase() || char.is_ascii_digit() || char == '_')
}

fn rust_fn_name(stem: &str) -> String {
    let name = format!("case_{stem}");
    if is_rust_keyword(&name) {
        format!("r#{name}")
    } else {
        name
    }
}

fn is_rust_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "gen"
            | "macro"
            | "override"
            | "priv"
            | "try"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
    )
}

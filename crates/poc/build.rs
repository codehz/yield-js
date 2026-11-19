use std::fs;
use std::path::{Path, PathBuf};

/// Walks `tests/cases` recursively and returns `(relative_path, category, stem)` for each `.js` case.
///
/// `category` is the case's directory name under `tests/cases` (empty for
/// top-level cases). Per-file stems stay validated against `valid_stem`; the
/// category must be lowercase ASCII like a stem.
fn collect_cases(cases_dir: &Path) -> Vec<(PathBuf, String, String)> {
    let mut cases = Vec::new();
    let mut walk = vec![cases_dir.to_path_buf()];
    while let Some(dir) = walk.pop() {
        let entries = fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()));
            let path = entry.path();
            let file_type = entry
                .file_type()
                .unwrap_or_else(|error| panic!("failed to stat {}: {error}", path.display()));
            if file_type.is_dir() {
                walk.push(path);
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("js")
            {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_else(|| panic!("invalid case file name {}", path.display()))
                .to_string();
            if !valid_stem(&stem) {
                panic!("invalid case file stem `{stem}` ({})", path.display());
            }
            let relative = path
                .strip_prefix(cases_dir)
                .unwrap_or_else(|error| panic!("failed to relativize {}: {error}", path.display()))
                .to_path_buf();
            let category = relative
                .parent()
                .and_then(|parent| parent.file_name().and_then(|name| name.to_str()))
                .map(str::to_string)
                .unwrap_or_default();
            if !category.is_empty() && !valid_stem(&category) {
                panic!("invalid case category `{category}` ({})", path.display());
            }
            cases.push((relative, category, stem));
        }
    }
    cases.sort_by(|left, right| left.0.cmp(&right.0));
    if cases.is_empty() {
        panic!("tests/cases contains no .js cases");
    }
    cases
}

fn main() {
    println!("cargo:rerun-if-changed=tests/cases");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let cases_dir = Path::new(&manifest_dir).join("tests/cases");
    if !cases_dir.is_dir() {
        panic!("tests/cases directory is missing");
    }

    let cases = collect_cases(&cases_dir);

    let mut generated = String::new();
    for (relative, category, stem) in &cases {
        let path = cases_dir.join(relative);
        let path_literal = format!(
            "{:?}",
            path.to_str()
                .unwrap_or_else(|| panic!("case path is not UTF-8: {}", path.display()))
        );
        // `run_case` keys on the path relative to `tests/cases`, so failures
        // keep the file's location even across categories.
        let name = format!(
            "{}.js",
            relative.to_str().unwrap_or_else(|| {
                panic!(
                    "case path is not UTF-8: {}",
                    cases_dir.join(relative).display()
                )
            })
        );
        let function = match category.as_str() {
            "" => rust_fn_name(stem),
            category => format!("case_{category}_{}", rust_fn_name(stem)),
        };
        generated.push_str(&format!(
            "#[test]\nfn {function}() {{\n    let source = include_str!({path_literal});\n    if let Err(error) = yield_poc::run_case(\"{name}\", source) {{\n        panic!(\"{{error}}\");\n    }}\n}}\n\n"
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

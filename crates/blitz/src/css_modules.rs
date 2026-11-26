//! CSS Modules are scoped by logical ID; only local and global composition
//! can be resolved without adding dependency edges to the module graph.

use std::collections::BTreeMap;
use std::fmt::Write;

use lightningcss::css_modules::{Config, CssModuleReference, Pattern};
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};

pub(super) fn transform(id: &str, text: &str) -> Result<(String, String), String> {
    let sheet = StyleSheet::parse(
        text,
        ParserOptions {
            filename: id.into(),
            css_modules: Some(Config {
                pattern: Pattern::parse("[hash]_[local]").expect("valid CSS Modules name pattern"),
                ..Config::default()
            }),
            error_recovery: false,
            ..ParserOptions::default()
        },
    )
    .map_err(|error| match error.loc {
        Some(loc) => format!("{id}:{}:{}: {}", loc.line + 1, loc.column, error.kind),
        None => format!("{id}: {}", error.kind),
    })?;
    let result = sheet
        .to_css(PrinterOptions {
            minify: false,
            analyze_dependencies: None,
            ..PrinterOptions::default()
        })
        .map_err(|error| format!("{id}: {error}"))?;

    let mut mapping = String::from("(() => { const classes = Object.create(null);");
    for (local, export) in result
        .exports
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeMap<_, _>>()
    {
        let mut classes = export.name;
        for reference in export.composes {
            match reference {
                CssModuleReference::Local { name } | CssModuleReference::Global { name } => {
                    classes.push(' ');
                    classes.push_str(&name);
                }
                CssModuleReference::Dependency { specifier, .. } => {
                    return Err(format!(
                        "unsupported syntax: cross-file CSS Modules composition ({id}: {specifier})"
                    ));
                }
            }
        }
        let key = serde_json::to_string(&local).expect("encode CSS Modules key");
        let value = serde_json::to_string(&classes).expect("encode CSS Modules value");
        write!(mapping, " classes[{key}] = {value};").expect("write CSS Modules mapping");
    }
    mapping.push_str(" return classes; })()");
    Ok((result.code, mapping))
}

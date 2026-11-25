//! Filesystem resolution checks both lexical requests and canonical paths against package boundaries.

use std::borrow::Cow;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::ModuleResolver;
use super::manifest::{self, Map, Value};
use crate::ModuleId;

const PACKAGE_ID_PREFIX: &str = "npm:";
const EXTENSIONS: &[&str] = &[".js", ".mjs", ".jsx", ".json"];
const INDEX_FILES: &[&str] = &["index.js", "index.mjs", "index.jsx"];

/// Resolves project files and the ESM `exports` of installed packages within one root.
pub struct NodeResolver {
    root: PathBuf,
}

impl NodeResolver {
    pub fn new(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|err| format!("{root:?}: {err}"))?;
        if !root.is_dir() {
            return Err(format!("source root is not a directory: {root:?}"));
        }
        Ok(Self { root })
    }

    fn checked_path(&self, path: &Path, package: Option<&Path>) -> Result<Option<PathBuf>, String> {
        let canonical = match path.canonicalize() {
            Ok(path) => path,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("{path:?}: {err}")),
        };
        let relative = canonical
            .strip_prefix(&self.root)
            .map_err(|_| format!("source path is outside the source root: {path:?}"))?;
        if package_scope(relative)? != package {
            return Err(format!(
                "source path crosses a node_modules package boundary: {path:?}"
            ));
        }
        Ok(Some(canonical))
    }

    fn manifest(&self, directory: &Path, package: Option<&Path>) -> Result<Option<Map>, String> {
        let path = directory.join("package.json");
        let Some(path) = self.checked_path(&path, package)? else {
            return Ok(None);
        };
        if !path.is_file() {
            return Err(format!("package manifest is not a file: {path:?}"));
        }
        let source = fs::read_to_string(&path).map_err(|err| format!("{path:?}: {err}"))?;
        manifest::parse(&source)
            .map(Some)
            .map_err(|err| format!("{path:?}: {err}"))
    }

    fn validate_source(
        &self,
        requested: &Path,
        file: &Path,
        package: Option<&Path>,
    ) -> Result<(), String> {
        // A symlink must not disguise a JSON or CommonJS file as ESM, or vice versa.
        for path in [requested, file] {
            match path.extension().and_then(|ext| ext.to_str()) {
                Some("json") => return Err(format!("JSON modules are unsupported: {path:?}")),
                Some("cjs") => return Err(format!("CommonJS modules are unsupported: {path:?}")),
                _ => {}
            }
        }
        match file.extension().and_then(|ext| ext.to_str()) {
            Some("mjs") => return Ok(()),
            Some("js" | "jsx") => {}
            _ => return Err(format!("unsupported source file type: {file:?}")),
        }
        let scope = package.map_or_else(|| self.root.clone(), |path| self.root.join(path));
        for directory in file
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(&scope))
        {
            if let Some(manifest) = self.manifest(directory, package)? {
                let kind = manifest.get("type").and_then(Value::as_str);
                if kind == Some("commonjs") || (package.is_some() && kind != Some("module")) {
                    return Err(format!("CommonJS modules are unsupported: {file:?}"));
                }
                break;
            }
        }
        Ok(())
    }

    fn probe(&self, relative: &str, package: Option<&Path>) -> Result<ModuleId, String> {
        if package_scope(Path::new(relative))? != package {
            return Err(format!(
                "source path crosses a node_modules package boundary: {relative:?}"
            ));
        }
        let path = self.root.join(relative);
        let mut is_directory = false;
        if let Some(file) = self.checked_path(&path, package)? {
            if file.is_file() {
                return self.source_id(&path, &file, package);
            }
            if !file.is_dir() {
                return Err(format!(
                    "source path is not an ordinary file or directory: {relative:?}"
                ));
            }
            is_directory = true;
        }
        // Do not append an extension to the root or to a package directory itself.
        if !relative.is_empty() && package != Some(Path::new(relative)) {
            for ext in EXTENSIONS {
                let candidate = self.root.join(format!("{relative}{ext}"));
                if let Some(id) = self.probe_file(&candidate, package)? {
                    return Ok(id);
                }
            }
        }
        if is_directory {
            for index in INDEX_FILES {
                if let Some(id) = self.probe_file(&path.join(index), package)? {
                    return Ok(id);
                }
            }
        }
        Err(format!("source file not found: {relative:?}"))
    }

    fn probe_file(
        &self,
        candidate: &Path,
        package: Option<&Path>,
    ) -> Result<Option<ModuleId>, String> {
        let Some(file) = self.checked_path(candidate, package)? else {
            return Ok(None);
        };
        if file.is_dir() {
            return Ok(None);
        }
        if !file.is_file() {
            return Err(format!(
                "source path is not an ordinary file: {candidate:?}"
            ));
        }
        self.source_id(candidate, &file, package).map(Some)
    }

    fn source_id(
        &self,
        requested: &Path,
        file: &Path,
        package: Option<&Path>,
    ) -> Result<ModuleId, String> {
        self.validate_source(requested, file, package)?;
        let relative = file.strip_prefix(&self.root).expect("checked source root");
        let mut id = if package.is_some() {
            PACKAGE_ID_PREFIX.to_owned()
        } else {
            String::new()
        };
        for (index, part) in relative.iter().enumerate() {
            let part = part.to_str().ok_or("source ID is not UTF-8")?;
            validate_segment(part)?;
            if index != 0 {
                id.push('/');
            }
            id.push_str(part);
        }
        Ok(id)
    }

    fn resolve_package(&self, referrer: &str, specifier: &str) -> Result<ModuleId, String> {
        let (name, subpath) = package_request(specifier)?;
        let directory = Path::new(referrer)
            .parent()
            .expect("nonempty relative referrer");
        for directory in directory.ancestors() {
            if is_node_modules(directory) {
                continue;
            }
            let package = directory.join("node_modules").join(name);
            let absolute = self.root.join(&package);
            let Some(found) = self.checked_path(&absolute, Some(&package))? else {
                continue;
            };
            if !found.is_dir() {
                return Err(format!("installed package is not a directory: {name:?}"));
            }
            let manifest = self
                .manifest(&found, Some(&package))?
                .ok_or_else(|| format!("package {name:?} has no package.json"))?;
            let exports = manifest
                .get("exports")
                .ok_or_else(|| format!("package {name:?} has no exports"))?;
            let target = export_target(exports, subpath)
                .map_err(|err| format!("package {name:?}: {err}"))?;
            let target = target
                .strip_prefix("./")
                .ok_or_else(|| format!("package export target must start with './': {target:?}"))?;
            let target = target.strip_suffix('/').unwrap_or(target);
            if !target.is_empty() {
                for part in target.split('/') {
                    validate_segment(part)?;
                    if part.eq_ignore_ascii_case("node_modules") {
                        return Err("package export target contains node_modules".into());
                    }
                }
            }
            // Module IDs use portable separators even on Windows; the installation path
            // qualifies the package name so different versions never lose their lookup context.
            let mut relative = package
                .iter()
                .map(|p| p.to_str().expect("UTF-8 package path"))
                .collect::<Vec<_>>()
                .join("/");
            if !target.is_empty() {
                relative.push('/');
                relative.push_str(target);
            }
            return self.probe(&relative, Some(&package));
        }
        Err(format!("package not found: {name:?}"))
    }
}

impl ModuleResolver for NodeResolver {
    fn resolve(&self, referrer: Option<&ModuleId>, specifier: &str) -> Result<ModuleId, String> {
        let (referrer, package) = match referrer {
            Some(id) => {
                let (path, package) = decode_id(id)?;
                (Some(path), package)
            }
            None => (None, None),
        };
        if referrer.is_none()
            || matches!(specifier, "." | "..")
            || specifier.starts_with("./")
            || specifier.starts_with("../")
        {
            let relative = normalize_source(referrer, specifier)?;
            return self.probe(&relative, package);
        }
        self.resolve_package(referrer.expect("import has a referrer"), specifier)
    }

    fn load(&self, id: &ModuleId) -> Result<Cow<'_, str>, String> {
        let (relative, package) = decode_id(id)?;
        let path = self.root.join(relative);
        let file = self
            .checked_path(&path, package)?
            .filter(|file| file.is_file())
            .ok_or_else(|| format!("source file not found: {id:?}"))?;
        self.validate_source(&path, &file, package)?;
        fs::read_to_string(&file)
            .map(Cow::Owned)
            .map_err(|err| format!("{file:?}: {err}"))
    }
}

fn normalize_source(referrer: Option<&str>, specifier: &str) -> Result<String, String> {
    if specifier.is_empty() || specifier.starts_with('/') || specifier.contains(['\\', ':', '\0']) {
        return Err(format!("invalid source path: {specifier:?}"));
    }
    let mut parts = Vec::new();
    if let Some((directory, _)) = referrer.and_then(|path| path.rsplit_once('/')) {
        parts.extend(directory.split('/'));
    }
    for part in specifier.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("source path escapes the source root")?;
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

fn is_node_modules(path: &Path) -> bool {
    path.file_name()
        .and_then(|part| part.to_str())
        .is_some_and(|part| part.eq_ignore_ascii_case("node_modules"))
}

fn validate_segment(part: &str) -> Result<(), String> {
    if matches!(part, "" | "." | "..") || part.contains(['\\', ':', '\0', '*', '?', '#', '%']) {
        return Err(format!("invalid path segment: {part:?}"));
    }
    Ok(())
}

fn package_scope(relative: &Path) -> Result<Option<&Path>, String> {
    // Pick the innermost installed package, including the scope for scoped names.
    for ancestor in relative.ancestors() {
        let Some(parent) = ancestor.parent() else {
            break;
        };
        if is_node_modules(parent) {
            if ancestor
                .file_name()
                .and_then(|part| part.to_str())
                .is_some_and(|part| part.starts_with('@'))
            {
                return Err(format!("invalid scoped package path: {relative:?}"));
            }
            return Ok(Some(ancestor));
        }
        if parent
            .file_name()
            .and_then(|part| part.to_str())
            .is_some_and(|part| part.starts_with('@'))
            && parent.parent().is_some_and(is_node_modules)
        {
            return Ok(Some(ancestor));
        }
        if is_node_modules(ancestor) {
            return Err(format!(
                "source path enters node_modules without a package: {relative:?}"
            ));
        }
    }
    Ok(None)
}

fn decode_id(id: &str) -> Result<(&str, Option<&Path>), String> {
    let (relative, is_package) = id
        .strip_prefix(PACKAGE_ID_PREFIX)
        .map_or((id, false), |relative| (relative, true));
    for part in relative.split('/') {
        validate_segment(part)?;
    }
    let package = package_scope(Path::new(relative))?;
    if is_package != package.is_some() {
        return Err(format!("invalid source module ID: {id:?}"));
    }
    Ok((relative, package))
}

fn package_request(specifier: &str) -> Result<(&str, &str), String> {
    let mut split = specifier.split('/');
    let first = split.next().expect("split returns at least one element");
    let known_builtin = matches!(
        first,
        "assert"
            | "async_hooks"
            | "buffer"
            | "child_process"
            | "cluster"
            | "console"
            | "constants"
            | "crypto"
            | "dgram"
            | "diagnostics_channel"
            | "dns"
            | "domain"
            | "events"
            | "fs"
            | "http"
            | "http2"
            | "https"
            | "module"
            | "net"
            | "os"
            | "path"
            | "perf_hooks"
            | "process"
            | "punycode"
            | "querystring"
            | "readline"
            | "repl"
            | "stream"
            | "string_decoder"
            | "sys"
            | "timers"
            | "tls"
            | "trace_events"
            | "tty"
            | "url"
            | "util"
            | "v8"
            | "vm"
            | "wasi"
            | "worker_threads"
            | "zlib"
            | "_http_agent"
            | "_http_client"
            | "_http_common"
            | "_http_incoming"
            | "_http_outgoing"
            | "_http_server"
            | "_stream_duplex"
            | "_stream_passthrough"
            | "_stream_readable"
            | "_stream_transform"
            | "_stream_wrap"
            | "_stream_writable"
            | "_tls_common"
            | "_tls_wrap"
    );
    if specifier.starts_with("node:") || known_builtin {
        return Err(format!("Node built-ins are unsupported: {specifier:?}"));
    }
    let mut end = first.len();
    if first.starts_with('@') {
        let name = split
            .next()
            .ok_or("scoped package name is missing its name")?;
        if first.len() == 1 || name.starts_with('@') {
            return Err(format!("invalid scoped package name: {specifier:?}"));
        }
        end += 1 + name.len();
    }
    for part in specifier.split('/') {
        validate_segment(part)?;
        if part.eq_ignore_ascii_case("node_modules") {
            return Err(format!("invalid package specifier: {specifier:?}"));
        }
    }
    Ok((&specifier[..end], specifier.get(end + 1..).unwrap_or("")))
}

fn export_target<'a>(exports: &'a Value, subpath: &str) -> Result<&'a str, String> {
    let key = if subpath.is_empty() {
        ".".to_owned()
    } else {
        format!("./{subpath}")
    };
    let target = match exports.as_object() {
        Some(map) if map.keys().any(|key| key.starts_with('.')) => {
            if map.keys().any(|key| key != "." && !key.starts_with("./")) {
                return Err("invalid or mixed package exports keys".into());
            }
            map.get(&key)
        }
        _ if subpath.is_empty() => Some(exports),
        _ => None,
    };
    let missing = || format!("package subpath {key:?} is not exported for import");
    match select_target(target.ok_or_else(&missing)?)? {
        Target::Path(path) => Ok(path),
        Target::Blocked | Target::Unmatched => Err(missing()),
    }
}

enum Target<'a> {
    Path(&'a str),
    Blocked,
    Unmatched,
}

fn select_target(target: &Value) -> Result<Target<'_>, String> {
    match target {
        Value::String(path) => Ok(Target::Path(path)),
        Value::Object(map) => conditions(map),
        Value::Null => Ok(Target::Blocked),
        _ => Err("unsupported package export target; expected a path or conditional object".into()),
    }
}

fn conditions(map: &Map) -> Result<Target<'_>, String> {
    if map.keys().any(|key| key.starts_with('.')) {
        return Err("invalid subpath in package export conditions".into());
    }
    for condition in ["import", "default"] {
        if let Some(target) = map.get(condition) {
            match select_target(target)? {
                Target::Unmatched => continue,
                selected => return Ok(selected),
            }
        }
    }
    Ok(Target::Unmatched)
}

//! Filesystem resolution checks both lexical requests and canonical paths against package boundaries.

mod commonjs;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use oxc::span::Span;

use super::ModuleResolver;
use super::ResolvedModule;
use super::manifest::{self, Map, Value};
use crate::ModuleId;
use crate::error::{CompileError, unsupported};

const PACKAGE_ID_PREFIX: &str = "npm:";
const FACTORY_ID_PREFIX: &str = "cjs-factory:";
const CONVERSION_STAGE: &str = "CommonJS conversion";
const EXTENSIONS: &[&str] = &[".js", ".mjs", ".jsx", ".json"];
const INDEX_FILES: &[&str] = &["index.js", "index.mjs", "index.jsx"];

/// Resolves project files and installed packages within one root.
pub struct NodeResolver {
    root: PathBuf,
    web_production: bool,
    resource_extensions: BTreeSet<String>,
    commonjs_sources: Mutex<BTreeMap<ModuleId, ModuleId>>,
}

impl NodeResolver {
    pub fn new(root: &Path) -> Result<Self, String> {
        Self::with_mode(root, false)
    }

    /// Resolves browser-oriented production npm sources, including CommonJS.
    pub fn web_production(root: impl AsRef<Path>) -> Result<Self, String> {
        Self::with_mode(root.as_ref(), true)
    }

    /// Allow explicitly named resource files without adding extension probes.
    /// A caller can wrap the UTF-8 text loaded for these IDs in a source module.
    pub fn with_resource_extension(mut self, extension: &str) -> Self {
        self.resource_extensions.insert(extension.to_owned());
        self
    }

    fn with_mode(root: &Path, web_production: bool) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|err| format!("{root:?}: {err}"))?;
        if !root.is_dir() {
            return Err(format!("source root is not a directory: {root:?}"));
        }
        Ok(Self {
            root,
            web_production,
            resource_extensions: BTreeSet::new(),
            commonjs_sources: Mutex::default(),
        })
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
    ) -> Result<bool, String> {
        // A symlink must not disguise a JSON or CommonJS file as ESM, or vice versa.
        for path in [requested, file] {
            match path.extension().and_then(|ext| ext.to_str()) {
                Some("json") => return Err(format!("JSON modules are unsupported: {path:?}")),
                Some("cjs") if !self.web_production => {
                    return Err(format!("CommonJS modules are unsupported: {path:?}"));
                }
                _ => {}
            }
        }
        let requested_ext = requested.extension().and_then(|ext| ext.to_str());
        let file_ext = file.extension().and_then(|ext| ext.to_str());
        let is_resource = |extension: Option<&str>| {
            extension.is_some_and(|ext| self.resource_extensions.contains(ext))
        };
        if is_resource(requested_ext) || is_resource(file_ext) {
            if requested_ext == file_ext {
                return Ok(false);
            }
            return Err(format!(
                "resource extension does not match target: {requested:?}"
            ));
        }
        match file.extension().and_then(|ext| ext.to_str()) {
            Some("mjs") => return Ok(false),
            Some("jsx") if self.web_production => return Ok(false),
            Some("cjs") => return Ok(true),
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
                if self.web_production {
                    return match manifest.get("type") {
                        None => Ok(package.is_some()),
                        Some(Value::String(kind)) if kind == "module" => Ok(false),
                        Some(Value::String(kind)) if kind == "commonjs" => Ok(true),
                        _ => Err(format!("unsupported package type: {file:?}")),
                    };
                }
                if kind == Some("commonjs") || (package.is_some() && kind != Some("module")) {
                    return Err(format!("CommonJS modules are unsupported: {file:?}"));
                }
                break;
            }
        }
        Ok(self.web_production && package.is_some())
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
            for ext in EXTENSIONS
                .iter()
                .copied()
                .chain(self.web_production.then_some(".cjs"))
            {
                let candidate = self.root.join(format!("{relative}{ext}"));
                if let Some(id) = self.probe_file(&candidate, package)? {
                    return Ok(id);
                }
            }
        }
        if is_directory {
            for index in INDEX_FILES
                .iter()
                .copied()
                .chain(self.web_production.then_some("index.cjs"))
            {
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
        let commonjs = self.validate_source(requested, file, package)?;
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
        if self.web_production {
            let mut sources = self.commonjs_sources.lock().expect("CommonJS sources");
            if commonjs && !sources.contains_key(&id) {
                sources.insert(factory_id(&id), id.clone());
                sources.insert(id.clone(), id.clone());
            } else if !commonjs && sources.remove(&id).is_some() {
                sources.remove(&factory_id(&id));
            }
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
            let target = if let Some(exports) = manifest.get("exports") {
                let target = export_target(exports, subpath, self.web_production)
                    .map_err(|err| format!("package {name:?}: {err}"))?;
                target.strip_prefix("./").ok_or_else(|| {
                    format!("package export target must start with './': {target:?}")
                })?
            } else if !self.web_production {
                return Err(format!("package {name:?} has no exports"));
            } else if !subpath.is_empty() {
                subpath
            } else if let Some(main) = manifest.get("main").and_then(Value::as_str) {
                let main = main.strip_prefix("./").unwrap_or(main);
                if main.is_empty() {
                    return Err(format!("package {name:?} has an empty main target"));
                }
                main
            } else {
                "index.js"
            };
            let target = target.strip_suffix('/').unwrap_or(target);
            if !target.is_empty() {
                for part in target.split('/') {
                    validate_segment(part)?;
                    if part.eq_ignore_ascii_case("node_modules") {
                        let kind = if manifest.contains_key("exports") {
                            "export target"
                        } else {
                            "target"
                        };
                        return Err(format!("package {kind} contains node_modules"));
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

    fn resolve_source(&self, referrer: Option<&str>, specifier: &str) -> Result<ModuleId, String> {
        match referrer {
            None => self.probe(&normalize_source(None, specifier)?, None),
            Some(id) => {
                let (path, package) = decode_id(id)?;
                if is_relative_request(specifier) {
                    self.probe(&normalize_source(Some(path), specifier)?, package)
                } else {
                    self.resolve_package(path, specifier)
                }
            }
        }
    }

    fn read_source(&self, id: &str) -> Result<String, String> {
        let (relative, package) = decode_id(id)?;
        let path = self.root.join(relative);
        let file = self
            .checked_path(&path, package)?
            .filter(|file| file.is_file())
            .ok_or_else(|| format!("source file not found: {id:?}"))?;
        self.validate_source(&path, &file, package)?;
        fs::read_to_string(&file).map_err(|err| format!("{file:?}: {err}"))
    }

    fn require_target(
        &self,
        id: &str,
        source: &str,
        (specifier, span): &(String, Span),
    ) -> Result<ModuleId, CompileError> {
        let target = self.resolve_source(Some(id), specifier).map_err(|reason| {
            CompileError::new(
                source,
                *span,
                format!("CommonJS dependency {specifier:?}: {reason}"),
            )
            .in_module(id)
        })?;
        if self.generated_source_origin(&target).is_none() {
            return Err(unsupported(source, *span, "require of an ES module").in_module(id));
        }
        Ok(target)
    }

    fn generate(
        &self,
        id: &str,
        origin: ModuleId,
        locate: &dyn Fn(String) -> CompileError,
    ) -> Result<String, CompileError> {
        let read = |id: &str| {
            self.read_source(id)
                .map_err(|reason| locate(reason).in_stage(CONVERSION_STAGE))
        };
        let converted_error = |error: CompileError| error.in_stage(CONVERSION_STAGE);
        if id.starts_with(FACTORY_ID_PREFIX) {
            let source = read(&origin)?;
            let factory = commonjs::analyze(&origin, &source).map_err(converted_error)?;
            let targets = factory
                .requires
                .iter()
                .map(|request| {
                    self.require_target(&origin, &source, request)
                        .map(|target| factory_id(&target))
                        .map_err(converted_error)
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(factory.render(&targets));
        }

        // For one facade, the least fixed point is the union of static names of
        // all factories reachable via forwarding edges, including cycles.
        let mut pending = vec![origin.clone()];
        let mut seen = BTreeSet::new();
        let mut names = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let source = read(&id)?;
            let factory = commonjs::analyze(&id, &source).map_err(converted_error)?;
            names.extend(factory.names);
            for index in factory.forwards {
                pending.push(
                    self.require_target(&id, &source, &factory.requires[index])
                        .map_err(converted_error)?,
                );
            }
        }
        Ok(facade(&origin, names))
    }
}

impl ModuleResolver for NodeResolver {
    fn resolve(
        &self,
        referrer: Option<&ModuleId>,
        specifier: &str,
    ) -> Result<ResolvedModule, String> {
        if self.web_production && specifier.starts_with(FACTORY_ID_PREFIX) {
            let sources = self.commonjs_sources.lock().expect("CommonJS sources");
            let origin = referrer.and_then(|id| sources.get(id));
            // Only generated factories and the target's own facade may import one.
            let target = sources
                .get(specifier)
                .filter(|target| {
                    origin.is_some_and(|origin| {
                        origin == *target
                            || referrer.is_some_and(|id| id.starts_with(FACTORY_ID_PREFIX))
                    })
                })
                .ok_or("private CommonJS factory")?;
            return Ok(ResolvedModule {
                id: specifier.to_owned(),
                is_framework: decode_id(target)?.1.is_some(),
            });
        }
        let original = referrer
            .filter(|id| id.starts_with(FACTORY_ID_PREFIX))
            .map(|id| {
                self.generated_source_origin(id)
                    .ok_or("unknown CommonJS factory")
            })
            .transpose()?;
        let id = self.resolve_source(
            original.as_ref().or(referrer).map(String::as_str),
            specifier,
        )?;
        let is_framework = decode_id(&id)?.1.is_some();
        Ok(ResolvedModule { id, is_framework })
    }

    fn load(&self, id: &ModuleId) -> Result<Cow<'_, str>, String> {
        self.load_for_compile(id, &CompileError::internal)
            .map_err(|error| {
                if error.module().is_empty() {
                    error.message().to_owned()
                } else {
                    error.to_string()
                }
            })
    }

    fn load_for_compile(
        &self,
        id: &ModuleId,
        locate: &dyn Fn(String) -> CompileError,
    ) -> Result<Cow<'_, str>, CompileError> {
        if let Some(origin) = self.generated_source_origin(id) {
            self.generate(id, origin, locate).map(Cow::Owned)
        } else {
            self.read_source(id).map(Cow::Owned).map_err(locate)
        }
    }

    fn generated_source_origin(&self, id: &str) -> Option<ModuleId> {
        if !self.web_production {
            return None;
        }
        self.commonjs_sources
            .lock()
            .expect("CommonJS sources")
            .get(id)
            .cloned()
    }
}

fn factory_id(origin: &str) -> ModuleId {
    format!("{FACTORY_ID_PREFIX}{origin}")
}

fn facade(origin: &str, names: BTreeSet<String>) -> String {
    let mut source = format!(
        "import {{ init }} from {};\nconst value = init();\nexport {{ value as default }};\n",
        js_string(&factory_id(origin))
    );
    for (index, name) in names
        .into_iter()
        .filter(|name| name != "default")
        .enumerate()
    {
        let name = js_string(&name);
        writeln!(
            source,
            "const name{index} = value[{name}]; export {{ name{index} as {name} }};"
        )
        .expect("writing to a String");
    }
    source
}

fn js_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '"' | '\\' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            '\u{0}'..='\u{1f}' | '\u{2028}' | '\u{2029}' => {
                write!(escaped, "\\u{:04x}", ch as u32).expect("writing to a String");
            }
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn is_relative_request(specifier: &str) -> bool {
    matches!(specifier, "." | "..") || specifier.starts_with("./") || specifier.starts_with("../")
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

fn export_target<'a>(exports: &'a Value, subpath: &str, web: bool) -> Result<&'a str, String> {
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
    match select_target(target.ok_or_else(missing)?, web)? {
        Target::Path(path) => Ok(path),
        Target::Blocked | Target::Unmatched => Err(missing()),
    }
}

enum Target<'a> {
    Path(&'a str),
    Blocked,
    Unmatched,
}

fn select_target(target: &Value, web: bool) -> Result<Target<'_>, String> {
    match target {
        Value::String(path) => Ok(Target::Path(path)),
        Value::Object(map) => conditions(map, web),
        Value::Null => Ok(Target::Blocked),
        _ => Err("unsupported package export target; expected a path or conditional object".into()),
    }
}

fn conditions(map: &Map, web: bool) -> Result<Target<'_>, String> {
    if map.keys().any(|key| key.starts_with('.')) {
        return Err("invalid subpath in package export conditions".into());
    }
    let conditions: &[&str] = if web {
        &["browser", "import", "production", "default"]
    } else {
        &["import", "default"]
    };
    for &condition in conditions {
        if let Some(target) = map.get(condition) {
            match select_target(target, web)? {
                Target::Unmatched => continue,
                selected => return Ok(selected),
            }
        }
    }
    Ok(Target::Unmatched)
}

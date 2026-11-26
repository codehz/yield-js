//! JavaScript host for a Blitz document. A session owns the VM and its live DOM;
//! snapshots carry the DOM in the VM's `yield:dom` module payload.

mod dom;
mod snapshot;
mod window;

use std::cell::{Ref, RefCell};
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blitz_dom::{BaseDocument, DocumentConfig, NodeId, UserAgentStylesheet};
use blitz_html::HtmlDocument;
use yield_binding_macros::host;
use yield_compiler::{Compiler, LoadedModule, MemoryResolver, NodeResolver, Project};
use yield_vm::{
    Absent, Build, BuildResource, Diagnostic, Diagnostics, Host, JsValue, RestoreOptions, Snapshot,
    Value, Vm, VmError,
};

const SHELL: &str = "<!doctype html><html><head></head><body><div id=\"root\"></div></body></html>";

#[derive(Clone, Copy)]
enum UserEvent {
    Click,
    Input,
}

impl UserEvent {
    fn name(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Input => "input",
        }
    }

    fn cancelable(self) -> bool {
        matches!(self, Self::Click)
    }
}

struct Dom {
    doc: BaseDocument,
    nodes: Vec<NodeId>,
    handles: BTreeMap<NodeId, u32>,
    pending_event: Option<(UserEvent, NodeId)>,
    resources: BTreeMap<String, Arc<[u8]>>,
}

impl Dom {
    fn new(resources: &[BuildResource]) -> Self {
        Self {
            doc: HtmlDocument::from_html(SHELL, document_config()).into_inner(),
            nodes: Vec::new(),
            handles: BTreeMap::new(),
            pending_event: None,
            resources: Self::index_resources(resources),
        }
    }

    fn empty(resources: &[BuildResource]) -> Self {
        Self {
            doc: BaseDocument::new(document_config()),
            nodes: Vec::new(),
            handles: BTreeMap::new(),
            pending_event: None,
            resources: Self::index_resources(resources),
        }
    }

    fn index_resources(resources: &[BuildResource]) -> BTreeMap<String, Arc<[u8]>> {
        resources
            .iter()
            .map(|resource| (resource.uri.clone(), Arc::clone(&resource.bytes)))
            .collect()
    }
}

fn document_config() -> DocumentConfig {
    DocumentConfig {
        ua_stylesheets: vec![UserAgentStylesheet::default_sheet()],
        ..Default::default()
    }
}

#[derive(Default)]
struct RenderDiagnostics(RefCell<Vec<String>>);

impl Diagnostics for RenderDiagnostics {
    fn report(&self, event: Diagnostic<'_>) {
        let message = match event {
            Diagnostic::Uncaught { diagnostic, .. } => diagnostic.to_string(),
            Diagnostic::StartupException { diagnostic } => format!("script threw: {diagnostic}"),
            Diagnostic::UnhandledRejection { reason, .. } => {
                format!("unhandled rejection: {reason}")
            }
        };
        self.0.borrow_mut().push(message);
    }
}

impl RenderDiagnostics {
    fn check<T>(&self, result: Result<T, VmError>) -> Result<T, String> {
        let mut errors = self.0.take();
        match result {
            Ok(value) if errors.is_empty() => return Ok(value),
            Ok(_) => {}
            Err(error) if !matches!(error, VmError::Thrown(_)) || errors.is_empty() => {
                errors.push(error.to_string());
            }
            Err(_) => {}
        }
        Err(errors.join("\n"))
    }
}

/// Compile a self-contained ESM with the Blitz DOM preload.
pub fn compile_module(source: &str) -> Result<Build, String> {
    let resolver =
        MemoryResolver::new([("main.mjs", source), ("dom.mjs", include_str!("dom.mjs"))])?;
    compile(Project::new("main.mjs", resolver).with_preload("dom.mjs"))
}

/// Execute a self-contained ESM and return its document, discarding the VM.
pub fn render_module(source: &str) -> Result<BaseDocument, String> {
    render(compile_module(source)?)
}

type BlitzPlatform = yield_vm::Platform<Absent, Absent, Rc<RenderDiagnostics>>;

/// A live VM and its private Blitz document.
pub struct BlitzSession {
    vm: Vm<BlitzPlatform>,
    dom: Rc<RefCell<Dom>>,
    diagnostics: Rc<RenderDiagnostics>,
}

impl BlitzSession {
    /// Start a fresh document and evaluate the build.
    pub fn new(build: Build) -> Result<Self, String> {
        Self::with_dom(Dom::new(&build.resources), |host| {
            Vm::new(build, host, Default::default())
        })
    }

    /// Restore a trusted snapshot with the same build, without evaluating modules.
    pub fn restore(
        snapshot: Snapshot,
        build: Build,
        options: RestoreOptions,
    ) -> Result<Self, String> {
        Self::with_dom(Dom::empty(&build.resources), |host| {
            Vm::restore(snapshot, build, host, options)
        })
    }

    fn with_dom(
        dom: Dom,
        create: impl FnOnce(Host<BlitzPlatform>) -> Result<Vm<BlitzPlatform>, VmError>,
    ) -> Result<Self, String> {
        let dom = Rc::new(RefCell::new(dom));
        let diagnostics = Rc::new(RenderDiagnostics::default());
        let host = host! {
            dts: "dom.d.ts",
            modules: [dom::DomModule],
            services: {
                Rc<RefCell<Dom>> => dom.clone(),
                diagnostics: diagnostics.clone(),
            },
        }?;
        let vm = diagnostics.check(create(host))?;
        Ok(Self {
            vm,
            dom,
            diagnostics,
        })
    }

    /// Invoke a public export.
    pub fn start(&mut self, entry: &str, args: Vec<JsValue>) -> Result<Value, String> {
        self.vm
            .start(entry, args)
            .map_err(|error| error.to_string())
    }

    /// Deliver a primary click to an element and drain VM work. Returns `false`
    /// if canceled. Node IDs are local to this document; reacquire them after restore.
    pub fn click(&mut self, target: NodeId) -> Result<bool, String> {
        let accepted = self.dispatch_user_event(UserEvent::Click, target)?;
        self.run_until_idle()?;
        Ok(accepted)
    }

    fn dispatch_user_event(&mut self, kind: UserEvent, target: NodeId) -> Result<bool, String> {
        {
            let mut dom = self.dom.borrow_mut();
            let node = dom
                .doc
                .get_node(target)
                .filter(|node| node.element_data().is_some())
                .ok_or_else(|| format!("{} target is not an element", kind.name()))?;
            let mut root = node;
            while let Some(parent) = root.parent.and_then(|id| dom.doc.get_node(id)) {
                root = parent;
            }
            if root.id != dom.doc.root_node().id {
                return Err(format!("{} target is detached", kind.name()));
            }
            dom.pending_event = Some((kind, target));
        }
        let result = self
            .vm
            .start_host_export("yield:dom", "dispatchUserEvent", vec![]);
        self.dom.borrow_mut().pending_event = None;
        result
            .map(|value| value == Value::boolean(true))
            .map_err(|error| error.to_string())
    }

    /// Capture the VM and DOM state together.
    pub fn snapshot(&self, roots: &[Value]) -> Result<Snapshot, String> {
        self.vm.snapshot(roots).map_err(|error| error.to_string())
    }

    /// Inspect the DOM without bypassing its script bindings.
    pub fn document(&self) -> Ref<'_, BaseDocument> {
        Ref::map(self.dom.borrow(), |dom| &dom.doc)
    }

    /// Resolve styles and layout at the given animation time.
    pub fn resolve_layout(&mut self, time: f64) {
        self.dom.borrow_mut().doc.resolve(time);
    }

    /// Run pending work and return any reported script errors.
    pub fn run_until_idle(&mut self) -> Result<(), String> {
        self.diagnostics.check(self.vm.run_until_idle())
    }

    fn into_document(self) -> Result<BaseDocument, String> {
        drop(self.vm);
        let dom = Rc::try_unwrap(self.dom).map_err(|_| "Blitz document is still shared")?;
        Ok(dom.into_inner().doc)
    }
}

const DOM_PRELOAD: &str = "yield:blitz-dom-preload";

fn load_css(id: &str, text: String) -> Result<LoadedModule<'static>, String> {
    if id.ends_with(".module.css") {
        return Err(format!("unsupported syntax: CSS Modules ({id})"));
    }
    let uri = format!("asset:{id}");
    let literal = serde_json::to_string(&uri).expect("encode CSS URI as a string literal");
    Ok(LoadedModule {
        source: format!("import {{ insertCss }} from 'yield:dom'; insertCss({literal});").into(),
        resources: vec![BuildResource {
            uri,
            bytes: text.into_bytes().into(),
        }],
    })
}

/// Compile the JSX fixture against its installed production npm sources.
pub fn compile_fixture() -> Result<Build, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/npm-react-app");
    let modules = root.join("node_modules");
    if !modules
        .try_exists()
        .map_err(|error| format!("{modules:?}: {error}"))?
    {
        return Err(
            "React fixture dependencies are missing; run `npm ci --prefix fixtures/npm-react-app`"
                .into(),
        );
    }
    let resolver = NodeResolver::web_production(root)?.with_loader("css", load_css);
    compile(
        Project::new("./src/blitz.jsx", resolver)
            .with_preload_source(DOM_PRELOAD, include_str!("dom.mjs")),
    )
}

/// Execute the JSX fixture and return its first rendered document, discarding the VM.
pub fn render_fixture() -> Result<BaseDocument, String> {
    render(compile_fixture()?)
}

fn compile(project: Project<'_>) -> Result<Build, String> {
    let compiler = Compiler::from_dts(include_str!("dom.d.ts")).map_err(|e| e.to_string())?;
    compiler.compile(project).map_err(|e| e.to_string())
}

fn render(build: Build) -> Result<BaseDocument, String> {
    let mut session = BlitzSession::new(build)?;
    session.run_until_idle()?;
    session.into_document()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "yield-blitz-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("creating {path:?}: {error}"),
                }
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn collects_only_reachable_css_in_preload_and_source_order() {
        let root = TempDir::new();
        let styles = [
            ("c.css", "p { color: red }"),
            ("b.css", "p { color: blue }"),
            ("a.css", "p { color: green }"),
        ];
        for (id, text) in styles.into_iter().chain([
            ("entry.js", "import './dep.js'; import './b.css';"),
            (
                "dep.js",
                "import './entry.js'; import './b.css'; import './a.css';",
            ),
            ("unused.css", "p { color: black }"),
        ]) {
            fs::write(root.0.join(id), text).unwrap();
        }
        let calls = Rc::new(RefCell::new(BTreeMap::new()));
        let resolver = NodeResolver::web_production(&root.0)
            .unwrap()
            .with_loader("css", {
                let calls = calls.clone();
                move |id, text| {
                    *calls.borrow_mut().entry(id.to_owned()).or_insert(0) += 1;
                    load_css(id, text)
                }
            });
        let compiler = Compiler::from_dts(include_str!("dom.d.ts")).unwrap();
        let compile = || {
            compiler
                .compile(
                    Project::new("entry.js", &resolver)
                        .with_preload_source("preload.js", "import './c.css';"),
                )
                .unwrap()
        };
        let build = compile();
        let uris: Vec<_> = build
            .resources
            .iter()
            .map(|item| item.uri.as_str())
            .collect();
        assert_eq!(uris, ["asset:c.css", "asset:b.css", "asset:a.css"]);
        for (resource, (_, text)) in build.resources.iter().zip(styles) {
            assert_eq!(&*resource.bytes, text.as_bytes());
        }
        assert_eq!(
            *calls.borrow(),
            BTreeMap::from(styles.map(|(id, _)| (id.into(), 1)))
        );
        build.validate().unwrap();

        let updated = "p { color: orange }";
        fs::write(root.0.join("b.css"), updated).unwrap();
        let rebuilt = compile();
        assert_eq!(&*rebuilt.resources[1].bytes, updated.as_bytes());
        assert_ne!(rebuilt.build_id, build.build_id);
        assert_eq!(
            *calls.borrow(),
            BTreeMap::from(styles.map(|(id, _)| (id.into(), 2)))
        );
        rebuilt.validate().unwrap();
    }

    #[test]
    fn rejects_css_modules_at_the_import_site() {
        let root = TempDir::new();
        fs::write(root.0.join("theme.module.css"), "").unwrap();
        fs::write(root.0.join("entry.js"), "\nimport './theme.module.css';").unwrap();
        let resolver = NodeResolver::web_production(&root.0)
            .unwrap()
            .with_loader("css", load_css);
        let error = Compiler::from_dts(include_str!("dom.d.ts"))
            .unwrap()
            .compile(Project::new("entry.js", resolver))
            .unwrap_err();
        assert_eq!(
            (error.module(), error.line(), error.column()),
            ("entry.js", 2, 8)
        );
        assert!(
            error
                .message()
                .contains("unsupported syntax: CSS Modules (theme.module.css)"),
            "{error}"
        );
    }
}

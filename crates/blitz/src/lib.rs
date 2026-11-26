//! JavaScript host for a Blitz document. A session owns the VM and its live DOM;
//! snapshots carry the DOM in the VM's `yield:dom` module payload.

mod css_modules;
mod dom;
mod snapshot;
mod timer;
mod window;

use std::cell::{Ref, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blitz_dom::{BaseDocument, DocumentConfig, NodeId, UserAgentStylesheet};
use blitz_html::HtmlDocument;
use yield_binding_macros::host;
use yield_compiler::{Compiler, LoadedModule, MemoryResolver, NodeResolver, Project};
use yield_vm::{
    Absent, Build, BuildResource, Diagnostic, Diagnostics, Host, JsValue, ObjectId, RestoreOptions,
    Snapshot, Value, Vm, VmError,
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
    nodes: BTreeMap<u32, NodeId>,
    next_handle: u32,
    detached: BTreeSet<NodeId>,
    snapshot_image: RefCell<Option<Result<snapshot::DomImage, yield_vm::binding::CodecError>>>,
    // Identity is absent only while the native tree is being restored.
    handles: BTreeMap<NodeId, Option<ObjectId>>,
    dirty_checkboxes: BTreeSet<NodeId>,
    pending_event: Option<(UserEvent, NodeId)>,
    resources: BTreeMap<String, Arc<[u8]>>,
}

impl Dom {
    fn new(resources: &[BuildResource]) -> Self {
        Self {
            doc: HtmlDocument::from_html(SHELL, document_config()).into_inner(),
            nodes: BTreeMap::new(),
            next_handle: 0,
            detached: BTreeSet::new(),
            snapshot_image: RefCell::new(None),
            handles: BTreeMap::new(),
            dirty_checkboxes: BTreeSet::new(),
            pending_event: None,
            resources: Self::index_resources(resources),
        }
    }

    fn empty(resources: &[BuildResource]) -> Self {
        Self {
            doc: BaseDocument::new(document_config()),
            nodes: BTreeMap::new(),
            next_handle: 0,
            detached: BTreeSet::new(),
            snapshot_image: RefCell::new(None),
            handles: BTreeMap::new(),
            dirty_checkboxes: BTreeSet::new(),
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

type BlitzPlatform = yield_vm::Platform<
    Absent,
    Absent,
    Rc<RenderDiagnostics>,
    Absent,
    Absent,
    Absent,
    Absent,
    timer::Timers,
>;

/// A live VM and its private Blitz document.
pub struct BlitzSession {
    vm: Vm<BlitzPlatform>,
    dom: Rc<RefCell<Dom>>,
    diagnostics: Rc<RenderDiagnostics>,
    timers: timer::Timers,
    automatic_gc_at: Option<u64>,
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
        let mut session = Self::with_dom(Dom::empty(&build.resources), |host| {
            Vm::restore(snapshot, build, host, options)
        })?;
        {
            let mut dom = session.dom.borrow_mut();
            let Dom { nodes, handles, .. } = &mut *dom;
            for (object, value) in session
                .vm
                .host_instance_slots::<dom::Node>(dom::NODE_INDEX_SLOT)
            {
                let yield_vm::ValueRef::Number(index) = value.view() else {
                    continue;
                };
                let handle = index as u32;
                let Some(identity) = nodes.get(&handle).and_then(|id| handles.get_mut(id)) else {
                    return Err("missing restored DOM node handle".into());
                };
                if index != f64::from(handle) || identity.replace(object).is_some() {
                    return Err("invalid restored DOM node handle".into());
                }
            }
            if handles.values().any(Option::is_none) {
                return Err("missing restored DOM wrapper".into());
            }
        }
        session
            .diagnostics
            .check(session.vm.start_recovered_operations())?;
        Ok(session)
    }

    fn with_dom(
        dom: Dom,
        create: impl FnOnce(Host<BlitzPlatform>) -> Result<Vm<BlitzPlatform>, VmError>,
    ) -> Result<Self, String> {
        let dom = Rc::new(RefCell::new(dom));
        let diagnostics = Rc::new(RenderDiagnostics::default());
        let timers = timer::Timers::default();
        let host = host! {
            dts: "dom.d.ts",
            modules: [dom::DomModule],
            services: {
                Rc<RefCell<Dom>> => dom.clone(),
                diagnostics: diagnostics.clone(),
                timer: timers.clone(),
            },
        }?;
        let vm = diagnostics.check(create(host))?;
        Ok(Self {
            vm,
            dom,
            diagnostics,
            timers,
            automatic_gc_at: None,
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
        let (accepted, _) = self.dispatch_user_event(UserEvent::Click, target)?;
        self.run_until_idle()?;
        Ok(accepted)
    }

    // The second result indicates that we already handled checkbox activation.
    fn dispatch_user_event(
        &mut self,
        kind: UserEvent,
        target: NodeId,
    ) -> Result<(bool, bool), String> {
        let activation = {
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
            let element = node.element_data().unwrap();
            let activation = (matches!(kind, UserEvent::Click)
                && dom::is_checkbox(element)
                && !element.has_attr(blitz_dom::local_name!("disabled")))
            .then(|| dom::checkbox_checked(element));
            let activation = activation.map(|checked| {
                let was_dirty = !dom.dirty_checkboxes.insert(target);
                dom::write_checkbox_checked(&mut dom.doc, target, !checked);
                (checked, was_dirty)
            });
            dom.pending_event = Some((kind, target));
            activation
        };
        let result = self
            .vm
            .start_host_export("yield:dom", "dispatchUserEvent", vec![]);
        let mut dom = self.dom.borrow_mut();
        dom.pending_event = None;
        let accepted = result.map(|value| value == Value::boolean(true));
        if let Some((checked, was_dirty)) = activation
            && !matches!(accepted, Ok(true))
        {
            dom::write_checkbox_checked(&mut dom.doc, target, checked);
            if !was_dirty {
                dom.dirty_checkboxes.remove(&target);
            }
        }
        accepted
            .map(|accepted| (accepted, activation.is_some()))
            .map_err(|error| error.to_string())
    }

    /// Capture the VM and DOM state together.
    pub fn snapshot(&self, roots: &[Value]) -> Result<Snapshot, String> {
        let dom = self.dom.borrow();
        let result = self.vm.snapshot_with_live_objects(roots, |live| {
            dom.snapshot_image
                .replace(Some(snapshot::DomImage::capture(&dom, live)));
        });
        dom.snapshot_image.take();
        result.map_err(|error| error.to_string())
    }

    /// Enable periodic idle collection. The caller must not retain unrooted
    /// VM values across idle checkpoints. Disabled by default for embedders.
    pub fn enable_automatic_gc(&mut self) {
        self.automatic_gc_at = Some(self.vm.allocation_totals().0);
    }

    /// Collect unreachable VM values and detached DOM trees. Call between VM
    /// turns with drained microtasks and pass any VM values you will reuse.
    pub fn collect_garbage(&mut self, roots: &[Value]) -> Result<(), String> {
        self.vm
            .collect_garbage(roots)
            .map_err(|error| error.to_string())?;
        self.dom
            .borrow_mut()
            .sweep(|object| self.vm.has_host_instance(object));
        if self.automatic_gc_at.is_some() {
            self.automatic_gc_at = Some(self.vm.allocation_totals().0);
        }
        Ok(())
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
        self.drain_turns().map(|_| ())
    }

    fn drain_turns(&mut self) -> Result<bool, String> {
        let mut woke = false;
        loop {
            self.diagnostics.check(self.vm.run_until_idle())?;
            let Some(token) = self.timers.due() else {
                if self
                    .automatic_gc_at
                    .is_some_and(|last| self.vm.allocation_totals().0.saturating_sub(last) >= 1024)
                {
                    self.collect_garbage(&[])?;
                }
                return Ok(woke);
            };
            self.diagnostics.check(self.vm.wake_timer(token))?;
            woke = true;
        }
    }

    fn into_document(self) -> Result<BaseDocument, String> {
        drop(self.vm);
        let dom = Rc::try_unwrap(self.dom).map_err(|_| "Blitz document is still shared")?;
        Ok(dom.into_inner().doc)
    }
}

const DOM_PRELOAD: &str = "yield:blitz-dom-preload";

fn load_css(id: &str, text: String) -> Result<LoadedModule<'static>, String> {
    let (text, mapping) = if id.ends_with(".module.css") {
        let (css, mapping) = css_modules::transform(id, &text)?;
        (css, Some(mapping))
    } else {
        (text, None)
    };
    let uri = format!("asset:{id}");
    let literal = serde_json::to_string(&uri).expect("encode CSS URI as a string literal");
    let mut source = format!("import {{ insertCss }} from 'yield:dom'; insertCss({literal});");
    if let Some(mapping) = mapping {
        source.push_str("\nexport default ");
        source.push_str(&mapping);
        source.push(';');
    }
    Ok(LoadedModule {
        source: source.into(),
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
    fn scopes_and_deduplicates_css_modules() {
        let root = TempDir::new();
        for dir in ["a", "b"] {
            fs::create_dir(root.0.join(dir)).unwrap();
        }
        for (path, source) in [
            (
                "entry.js",
                "import a from './a/theme.module.css';
                 import again from './a/../a/theme.module.css';
                 import b from './b/theme.module.css';
                 import empty from './empty.module.css';
                 export function inspect() {
                     return JSON.stringify({ a, b, empty, same: a === again,
                         nullPrototype: Object.getPrototypeOf(a) === null });
                 }",
            ),
            (
                "a/theme.module.css",
                r#".row { color: red; }
                   .row:hover { content: ".row"; background-image: url("../icon.svg"); }
                   :global(.shared) { color: blue; }
                   .__proto__ { composes: row; composes: shared utility from global; }"#,
            ),
            ("b/theme.module.css", ".row { color: green; }"),
            ("empty.module.css", ""),
        ] {
            fs::write(root.0.join(path), source).unwrap();
        }
        let resolver = NodeResolver::web_production(&root.0)
            .unwrap()
            .with_loader("css", load_css);
        let compiler = Compiler::from_dts(include_str!("dom.d.ts")).unwrap();
        let compile = || {
            compiler
                .compile(Project::new("entry.js", &resolver))
                .unwrap()
        };
        let build = compile();
        assert_eq!(build.to_bytes(), compile().to_bytes());
        let [a, b, empty] = build.resources.as_slice() else {
            panic!("expected one resource per distinct CSS module");
        };
        assert_eq!(a.uri, "asset:a/theme.module.css");
        assert_eq!(b.uri, "asset:b/theme.module.css");
        assert_eq!(empty.uri, "asset:empty.module.css");
        let a_css = std::str::from_utf8(&a.bytes).unwrap().to_owned();
        let b_css = std::str::from_utf8(&b.bytes).unwrap().to_owned();

        let mut session = BlitzSession::new(build).unwrap();
        let result = session.start("inspect", vec![]).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&result.into_string().unwrap().to_utf8().unwrap()).unwrap();
        assert_eq!(json["same"], true);
        assert_eq!(json["nullPrototype"], true);
        assert_eq!(json["empty"], serde_json::json!({}));
        assert_eq!(json["a"].as_object().unwrap().len(), 2);
        let a_row = json["a"]["row"].as_str().unwrap();
        let b_row = json["b"]["row"].as_str().unwrap();
        assert_ne!(a_row, b_row);
        assert!(a_css.starts_with(&format!(".{a_row} {{")), "{a_css}");
        assert!(a_css.contains(&format!(".{a_row}:hover {{")), "{a_css}");
        assert!(b_css.starts_with(&format!(".{b_row} {{")), "{b_css}");
        assert!(a_css.contains(".shared {"), "{a_css}");
        assert!(a_css.contains(r#"content: ".row""#), "{a_css}");
        assert!(a_css.contains(r#"url("../icon.svg")"#), "{a_css}");
        let composed = json["a"]["__proto__"].as_str().unwrap();
        let (own, rest) = composed.split_once(' ').unwrap();
        assert!(own.ends_with("___proto__"), "{composed}");
        assert_eq!(rest, format!("{a_row} shared utility"));
        let doc = session.document();
        let head = doc
            .root_element()
            .children
            .iter()
            .filter_map(|&id| doc.get_node(id))
            .find(|node| {
                node.element_data()
                    .is_some_and(|el| &*el.name.local == "head")
            })
            .unwrap();
        assert_eq!(head.children.len(), 3, "each module inserts one style node");
    }

    #[test]
    fn reports_css_module_errors_at_the_import_site() {
        let root = TempDir::new();
        fs::write(root.0.join("entry.js"), "\nimport './theme.module.css';").unwrap();
        let resolver = NodeResolver::web_production(&root.0)
            .unwrap()
            .with_loader("css", load_css);
        let compiler = Compiler::from_dts(include_str!("dom.d.ts")).unwrap();
        for (css, expected) in [
            ("\n.row, { color: red; }", "theme.module.css:2:"),
            (
                ".row { composes: base from './other.module.css'; }",
                "unsupported syntax: cross-file CSS Modules composition",
            ),
        ] {
            fs::write(root.0.join("theme.module.css"), css).unwrap();
            let error = compiler
                .compile(Project::new("entry.js", &resolver))
                .unwrap_err();
            assert_eq!(
                (error.module(), error.line(), error.column()),
                ("entry.js", 2, 8)
            );
            assert!(error.message().contains(expected), "{error}");
            assert!(error.message().contains("theme.module.css"), "{error}");
        }
    }
}

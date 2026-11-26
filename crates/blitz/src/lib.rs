//! JavaScript host for a Blitz document. A session owns the VM and its live DOM;
//! snapshots carry the DOM in the VM's `yield:dom` module payload.

mod dom;
mod snapshot;
mod window;

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blitz_dom::{BaseDocument, DocumentConfig, NodeId, UserAgentStylesheet};
use blitz_html::HtmlDocument;
use yield_binding_macros::host;
use yield_compiler::{
    BuildTransform, CompileError, Compiler, MemoryResolver, ModuleId, ModuleResolver, NodeResolver,
    Project, ResolvedModule, TransformError,
};
use yield_vm::{
    Absent, Build, BuildResource, Diagnostic, Diagnostics, Host, JsValue, RestoreOptions, Snapshot,
    Value, Vm, VmError,
};

const SHELL: &str = "<!doctype html><html><head></head><body><div id=\"root\"></div></body></html>";

struct Dom {
    doc: BaseDocument,
    nodes: Vec<NodeId>,
    handles: BTreeMap<NodeId, u32>,
    click_target: Option<NodeId>,
    resources: BTreeMap<String, Arc<[u8]>>,
}

impl Dom {
    fn new(resources: &[BuildResource]) -> Self {
        Self {
            doc: HtmlDocument::from_html(SHELL, document_config()).into_inner(),
            nodes: Vec::new(),
            handles: BTreeMap::new(),
            click_target: None,
            resources: Self::index_resources(resources),
        }
    }

    fn empty(resources: &[BuildResource]) -> Self {
        Self {
            doc: BaseDocument::new(document_config()),
            nodes: Vec::new(),
            handles: BTreeMap::new(),
            click_target: None,
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
        let accepted = self.dispatch_click(target)?;
        self.run_until_idle()?;
        Ok(accepted)
    }

    fn dispatch_click(&mut self, target: NodeId) -> Result<bool, String> {
        {
            let mut dom = self.dom.borrow_mut();
            let node = dom
                .doc
                .get_node(target)
                .filter(|node| node.element_data().is_some())
                .ok_or("click target is not an element")?;
            let mut root = node;
            while let Some(parent) = root.parent.and_then(|id| dom.doc.get_node(id)) {
                root = parent;
            }
            if root.id != dom.doc.root_node().id {
                return Err("click target is detached".into());
            }
            dom.click_target = Some(target);
        }
        let result = self
            .vm
            .start_host_export("yield:dom", "dispatchClick", vec![]);
        self.dom.borrow_mut().click_target = None;
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

type CssResources = RefCell<BTreeMap<ModuleId, BuildResource>>;

struct FixtureResolver<'a>(NodeResolver, &'a CssResources);

impl FixtureResolver<'_> {
    fn load_css(&self, id: &ModuleId) -> Result<Cow<'_, str>, String> {
        if id.ends_with(".module.css") {
            return Err(format!("unsupported syntax: CSS Modules ({id})"));
        }
        let uri = format!("asset:{id}");
        if !self.1.borrow().contains_key(id) {
            // A canonical ID is defined once: later imports reuse the first
            // resource and never reread the file.
            let bytes = self.0.load(id)?.as_bytes().into();
            self.1.borrow_mut().insert(
                id.clone(),
                BuildResource {
                    uri: uri.clone(),
                    bytes,
                },
            );
        }
        let literal = serde_json::to_string(&uri).expect("encode CSS URI as a string literal");
        Ok(Cow::Owned(format!(
            "import {{ insertCss }} from 'yield:dom'; insertCss({literal});"
        )))
    }
}

impl ModuleResolver for FixtureResolver<'_> {
    fn resolve(
        &self,
        referrer: Option<&ModuleId>,
        specifier: &str,
    ) -> Result<ResolvedModule, String> {
        if referrer.is_none() && specifier == DOM_PRELOAD {
            return Ok(ResolvedModule {
                id: DOM_PRELOAD.into(),
                is_framework: false,
            });
        }
        self.0.resolve(referrer, specifier)
    }

    fn load(&self, id: &ModuleId) -> Result<Cow<'_, str>, String> {
        if id == DOM_PRELOAD {
            return Ok(Cow::Borrowed(include_str!("dom.mjs")));
        }
        if id.ends_with(".css") {
            return self.load_css(id);
        }
        self.0.load(id)
    }

    fn load_for_compile(
        &self,
        id: &ModuleId,
        locate: &dyn Fn(String) -> CompileError,
    ) -> Result<Cow<'_, str>, CompileError> {
        if id == DOM_PRELOAD || id.ends_with(".css") {
            return self.load(id).map_err(locate);
        }
        self.0.load_for_compile(id, locate)
    }

    fn generated_source_origin(&self, id: &str) -> Option<ModuleId> {
        self.0.generated_source_origin(id)
    }
}

struct CollectCss<'a>(&'a CssResources);

impl BuildTransform for CollectCss<'_> {
    fn transform(&mut self, build: &mut Build) -> Result<(), TransformError> {
        let mut cached = self.0.take();
        let mut visited = vec![false; build.modules.len()];
        let mut pending = vec![build.entry];
        pending.extend(build.preload);
        while let Some(index) = pending.pop() {
            if std::mem::replace(&mut visited[index as usize], true) {
                continue;
            }
            let module = &build.modules[index as usize];
            if module.id.ends_with(".css") {
                let resource = cached.remove(&module.id).ok_or_else(|| {
                    TransformError::new(format!("missing CSS resource: asset:{}", module.id))
                })?;
                build.resources.push(resource);
            }
            pending.extend(module.dependencies.iter().rev().copied());
        }
        Ok(())
    }
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
    let resources = CssResources::default();
    let resolver = FixtureResolver(
        NodeResolver::web_production(root)?.with_resource_extension("css"),
        &resources,
    );
    compile(
        Project::new("./src/blitz.jsx", resolver)
            .with_preload(DOM_PRELOAD)
            .with_build_transform(CollectCss(&resources)),
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

    #[test]
    fn collects_only_reachable_css_in_preload_and_source_order() {
        let resolver = MemoryResolver::new([
            ("preload.js", "import './c.css';"),
            ("entry.js", "import './dep.js'; import './b.css';"),
            (
                "dep.js",
                "import './entry.js'; import './b.css'; import './a.css';",
            ),
            ("a.css", ""),
            ("b.css", ""),
            ("c.css", ""),
        ])
        .unwrap();
        let resources: CssResources = ["a.css", "b.css", "c.css", "unused.css"]
            .into_iter()
            .map(|id| {
                (
                    id.into(),
                    BuildResource {
                        uri: format!("asset:{id}"),
                        bytes: [].into(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>()
            .into();
        let build = Compiler::default()
            .compile(
                Project::new("entry.js", resolver)
                    .with_preload("preload.js")
                    .with_build_transform(CollectCss(&resources)),
            )
            .unwrap();
        let uris: Vec<_> = build
            .resources
            .iter()
            .map(|item| item.uri.as_str())
            .collect();
        assert_eq!(uris, ["asset:c.css", "asset:b.css", "asset:a.css"]);
        build.validate().unwrap();
    }
}

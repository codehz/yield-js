//! JavaScript host for a Blitz document. A session owns the VM and its live DOM;
//! snapshots carry the DOM in the VM's `yield:dom` module payload.

mod dom;
mod snapshot;

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use blitz_dom::{BaseDocument, DocumentConfig, NodeId, UserAgentStylesheet};
use blitz_html::HtmlDocument;
use yield_binding_macros::host;
use yield_compiler::{
    CompileError, Compiler, MemoryResolver, ModuleId, ModuleResolver, NodeResolver, Project,
    ResolvedModule,
};
use yield_vm::{
    Absent, Build, Diagnostic, Diagnostics, Host, JsValue, RestoreOptions, Snapshot, Value, Vm,
    VmError,
};

const SHELL: &str = "<!doctype html><html><head></head><body><div id=\"root\"></div></body></html>";

struct Dom {
    doc: BaseDocument,
    nodes: Vec<NodeId>,
    handles: BTreeMap<NodeId, u32>,
}

impl Dom {
    fn new() -> Self {
        Self {
            doc: HtmlDocument::from_html(SHELL, document_config()).into_inner(),
            nodes: Vec::new(),
            handles: BTreeMap::new(),
        }
    }

    fn empty() -> Self {
        Self {
            doc: BaseDocument::new(document_config()),
            nodes: Vec::new(),
            handles: BTreeMap::new(),
        }
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
        Self::with_dom(Dom::new(), |host| Vm::new(build, host, Default::default()))
    }

    /// Restore a trusted snapshot with the same build, without evaluating modules.
    pub fn restore(
        snapshot: Snapshot,
        build: Build,
        options: RestoreOptions,
    ) -> Result<Self, String> {
        Self::with_dom(Dom::empty(), |host| {
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

    /// Capture the VM and DOM state together.
    pub fn snapshot(&self, roots: &[Value]) -> Result<Snapshot, String> {
        self.vm.snapshot(roots).map_err(|error| error.to_string())
    }

    /// Inspect the DOM without bypassing its script bindings.
    pub fn document(&self) -> Ref<'_, BaseDocument> {
        Ref::map(self.dom.borrow(), |dom| &dom.doc)
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

struct FixtureResolver(NodeResolver);

impl ModuleResolver for FixtureResolver {
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
        self.0.load(id)
    }

    fn load_for_compile(
        &self,
        id: &ModuleId,
        locate: &dyn Fn(String) -> CompileError,
    ) -> Result<Cow<'_, str>, CompileError> {
        if id == DOM_PRELOAD {
            return self.load(id).map_err(locate);
        }
        self.0.load_for_compile(id, locate)
    }

    fn generated_source_origin(&self, id: &str) -> Option<ModuleId> {
        self.0.generated_source_origin(id)
    }
}

/// Compile the first-mount JSX fixture against its installed production npm sources.
pub fn render_fixture() -> Result<BaseDocument, String> {
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
    let resolver = FixtureResolver(NodeResolver::web_production(root)?);
    render(compile(
        Project::new("./src/blitz.jsx", resolver).with_preload(DOM_PRELOAD),
    )?)
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

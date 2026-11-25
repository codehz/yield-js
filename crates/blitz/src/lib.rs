//! One-shot JavaScript host for a Blitz HTML document.

mod dom;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use blitz_dom::{DocumentConfig, NodeId, UserAgentStylesheet};
use blitz_html::HtmlDocument;
use yield_binding_macros::host_bindings;
use yield_compiler::{
    CompileError, Compiler, MemoryResolver, ModuleId, ModuleResolver, NodeResolver, Project,
    ResolvedModule,
};
use yield_vm::{Bindings, Host, JsString, JsValue, ServiceKey, Vm, VmError};

const SHELL: &str = "<!doctype html><html><head></head><body><div id=\"root\"></div></body></html>";

struct Dom {
    doc: HtmlDocument,
    nodes: Vec<NodeId>,
    handles: BTreeMap<NodeId, u32>,
}

impl Dom {
    fn new() -> Self {
        Self {
            doc: HtmlDocument::from_html(
                SHELL,
                DocumentConfig {
                    ua_stylesheets: vec![UserAgentStylesheet::default_sheet()],
                    ..Default::default()
                },
            ),
            nodes: Vec::new(),
            handles: BTreeMap::new(),
        }
    }
}

struct DomKey;

impl ServiceKey for DomKey {
    type Value = Rc<RefCell<Dom>>;
    const ID: &'static str = "yield:dom/document";
    const VERSION: u32 = 1;
    // Live per-run state, not a platform capability that snapshots restore.
    const IN_FINGERPRINT: bool = false;
}

struct DomHost {
    bindings: Rc<Bindings>,
    errors: Vec<String>,
}

impl Host for DomHost {
    fn bindings(&self) -> Rc<Bindings> {
        self.bindings.clone()
    }

    fn report_error(&mut self, error: JsValue) {
        self.errors.push(error.to_string());
    }

    fn report_error_with_diagnostic(&mut self, _error: JsValue, diagnostic: &JsString) {
        self.errors.push(diagnostic.to_string());
    }

    fn report_startup_exception(&mut self, diagnostic: &JsString) {
        self.errors.push(format!("script threw: {diagnostic}"));
    }

    fn unhandled_rejection(&mut self, _promise: JsValue, reason: JsValue) {
        self.errors.push(format!("unhandled rejection: {reason}"));
    }
}

/// Execute one self-contained ESM and return the document it mutated.
/// The VM and the script's event listeners are discarded before returning.
pub fn render_module(source: &str) -> Result<HtmlDocument, String> {
    let resolver =
        MemoryResolver::new([("main.mjs", source), ("dom.mjs", include_str!("dom.mjs"))])?;
    render(Project::new("main.mjs", resolver).with_preload("dom.mjs"))
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
pub fn render_fixture() -> Result<HtmlDocument, String> {
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
    render(Project::new("./src/blitz.jsx", resolver).with_preload(DOM_PRELOAD))
}

fn render(project: Project<'_>) -> Result<HtmlDocument, String> {
    let compiler = Compiler::from_dts(include_str!("dom.d.ts")).map_err(|e| e.to_string())?;
    let build = compiler.compile(project).map_err(|e| e.to_string())?;
    let dom = Rc::new(RefCell::new(Dom::new()));
    let bindings = host_bindings! {
        dts: "dom.d.ts",
        modules: [dom::DomModule],
        services: {
            DomKey => dom.clone(),
        },
    }?;
    let mut host = DomHost {
        bindings: Rc::new(bindings),
        errors: Vec::new(),
    };
    let result = Vm::new(build, &mut host, Default::default())
        .and_then(|mut vm| vm.run_until_idle(&mut host));
    if let Err(error) = result
        && (!matches!(error, VmError::Thrown(_)) || host.errors.is_empty())
    {
        host.errors.push(error.to_string());
    }
    let errors = std::mem::take(&mut host.errors);
    drop(host);
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    let dom = Rc::try_unwrap(dom).map_err(|_| "Blitz document is still shared".to_owned())?;
    Ok(dom.into_inner().doc)
}

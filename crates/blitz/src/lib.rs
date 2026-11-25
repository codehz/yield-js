//! One-shot JavaScript host for a Blitz HTML document.

mod dom;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use blitz_dom::{DocumentConfig, NodeId, UserAgentStylesheet};
use blitz_html::HtmlDocument;
use yield_binding_macros::host_bindings;
use yield_compiler::{Compiler, MemoryResolver, Project};
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
    let compiler = Compiler::from_dts(include_str!("dom.d.ts")).map_err(|e| e.to_string())?;
    let resolver =
        MemoryResolver::new([("main.mjs", source), ("dom.mjs", include_str!("dom.mjs"))])?;
    let build = compiler
        .compile(Project::new("main.mjs", resolver).with_preload("dom.mjs"))
        .map_err(|e| e.to_string())?;
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

//! `yield:dom` host module: DOM node wrappers bound to the Blitz document.
//!
//! `resolver.rs` holds the shared `Dom` helpers that map between node ids
//! and JS handle instances.

// Each class owns one or more tags. `from module` names an existing binding.
macro_rules! html_elements {
    ($consumer:ident $($extra:tt)*) => {
        $consumer! {
            {
                (["iframe"], HTMLIFrameElement, construct_iframe),
                (["input"], HTMLInputElement, construct_input, from input),
                (["button"], HTMLButtonElement, construct_button),
                (["label"], HTMLLabelElement, construct_label),
                (["ul"], HTMLUListElement, construct_ul),
                (["li"], HTMLLIElement, construct_li),
                (["div"], HTMLDivElement, construct_div),
                (["span"], HTMLSpanElement, construct_span),
                (["h1", "h2", "h3", "h4", "h5", "h6"], HTMLHeadingElement, construct_heading),
                (["p"], HTMLParagraphElement, construct_paragraph),
                (["ol"], HTMLOListElement, construct_ol),
                (["blockquote", "q"], HTMLQuoteElement, construct_quote),
                (["pre"], HTMLPreElement, construct_pre),
                (["br"], HTMLBRElement, construct_br),
                (["hr"], HTMLHRElement, construct_hr),
            }
            $($extra)*
        }
    };
}

macro_rules! declare_html_element {
    ($class:ident) => {
        #[yield_binding_macros::host_class(tag, parent = element::HTMLElement)]
        struct $class;
    };
    ($class:ident, from $owner:ident) => {};
}

macro_rules! declare_html_elements {
    ({ $(($tags:tt, $class:ident, $ctor:ident $(, from $owner:ident)?),)* }) => {
        $(declare_html_element!($class $(, from $owner)?);)*
    };
}

html_elements!(declare_html_elements);

macro_rules! receiver_index {
    ($cx:ident, $this:ident) => {{
        let wrapper = $this.instance().value($cx);
        super::base_node_index($cx, &wrapper)
    }};
}

mod document;
mod element;
mod input;
mod module;
mod node;
mod resolver;

use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::NodeData;
use yield_vm::binding::CallCx;
use yield_vm::intrinsics::TypeError;
use yield_vm::{Instance, JsError, JsResult, JsValue};

use self::node::NodeSlots;
use super::Dom;

pub(crate) use input::{
    checkbox_checked, is_checkbox, is_text_input, write_checkbox_checked, write_input_value,
};
pub use module::DomModule;
pub(crate) use node::Node;

/// Resolve the numeric node index stored on a `Node` instance slot.
pub(super) fn node_index(cx: &mut CallCx<'_>, node: Instance<Node>) -> JsResult<u32> {
    let Some(index) = node.index(cx)?.as_number() else {
        let error =
            cx.construct::<TypeError>(&[JsValue::string("DOM node is not bound to a document")])?;
        return Err(cx.throw(error));
    };
    Ok(index as u32)
}

/// Resolve the `Node` instance behind an `Element`/`Document` subclass value
/// and return its node index.
pub(super) fn base_node_index(cx: &mut CallCx<'_>, value: &JsValue) -> JsResult<u32> {
    let node = cx.instance::<Node>(value)?;
    node_index(cx, node)
}

fn node_name(dom: &Rc<RefCell<Dom>>, index: u32) -> JsResult<JsValue> {
    let dom = dom.borrow();
    let node = dom.node(index)?;
    Ok(match &node.data {
        NodeData::Element(element) => JsValue::string(element.name.local.to_uppercase()),
        NodeData::Text(_) => JsValue::string("#text"),
        NodeData::Comment { .. } => JsValue::string("#comment"),
        NodeData::Document(_) => JsValue::string("#document"),
        _ => return Err(JsError::new::<TypeError>("unsupported DOM node kind")),
    })
}

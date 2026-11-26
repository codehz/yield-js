//! `yield:dom` host module: DOM node wrappers bound to the Blitz document.
//!
//! Each class file keeps one host class; `resolver.rs` holds the shared
//! `Dom` helpers that map between node ids and JS handle instances.

mod document;
mod element;
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

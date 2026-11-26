//! Shared `Dom` helpers: id→handle resolution and wrapper initialization.

use blitz_dom::{NodeData, NodeId, ns};
use yield_vm::binding::CallCx;
use yield_vm::intrinsics::TypeError;
use yield_vm::{JsError, JsResult, JsValue};

use super::Dom;
use super::document::{Document, DocumentSlots};
use super::element::{
    Element, HTMLButtonElement, HTMLElement, HTMLIFrameElement, HTMLLIElement, HTMLLabelElement,
    HTMLUListElement, alloc_element,
};
use super::input::HTMLInputElement;
use super::node::{Node, NodeSlots};

impl Dom {
    /// Resolve a handle index to the Blitz node it wraps.
    pub(crate) fn node(&self, index: u32) -> JsResult<&blitz_dom::Node> {
        self.nodes
            .get(index as usize)
            .and_then(|id| self.doc.get_node(*id))
            .ok_or_else(|| JsError::new::<TypeError>("invalid DOM node handle"))
    }

    /// Resolve a Blitz node id to a JS handle, creating (and caching) the
    /// wrapper instance for its concrete kind when first seen.
    pub(crate) fn resolve_node(
        &mut self,
        cx: &mut CallCx<'_>,
        owner: JsValue,
        id: Option<NodeId>,
    ) -> JsResult<JsValue> {
        let Some(id) = id else {
            return Ok(JsValue::null());
        };
        let document = cx.instance::<Document>(&owner)?;
        if let Some(&index) = self.handles.get(&id) {
            return document.nodes_get(cx, index as usize);
        }

        // Intern missing ancestors first so even the shell's original nodes
        // have a VM-managed event parent when first wrapped.
        let mut missing = Vec::new();
        let mut current = Some(id);
        let mut parent = loop {
            let Some(id) = current else {
                break JsValue::null();
            };
            if let Some(&index) = self.handles.get(&id) {
                break document.nodes_get(cx, index as usize)?;
            }
            let node = self
                .doc
                .get_node(id)
                .ok_or_else(|| JsError::new::<TypeError>("invalid DOM node handle"))?;
            missing.push(id);
            current = node.parent;
        };
        for id in missing.into_iter().rev() {
            let index = u32::try_from(self.nodes.len())
                .map_err(|_| JsError::new::<TypeError>("too many DOM nodes"))?;
            let value = match &self.doc.get_node(id).expect("collected live ancestor").data {
                NodeData::Element(element) if element.name.ns == ns!(html) => {
                    match element.name.local.as_ref() {
                        "iframe" => alloc_element(cx, HTMLIFrameElement)?.value(cx),
                        "input" => alloc_element(cx, HTMLInputElement)?.value(cx),
                        "button" => alloc_element(cx, HTMLButtonElement)?.value(cx),
                        "label" => alloc_element(cx, HTMLLabelElement)?.value(cx),
                        "ul" => alloc_element(cx, HTMLUListElement)?.value(cx),
                        "li" => alloc_element(cx, HTMLLIElement)?.value(cx),
                        _ => alloc_element(cx, HTMLElement)?.value(cx),
                    }
                }
                NodeData::Element(_) => alloc_element(cx, Element)?.value(cx),
                NodeData::Text(_) | NodeData::Comment { .. } => cx.alloc(Node)?.value(cx),
                _ => return Err(JsError::new::<TypeError>("unsupported DOM node kind")),
            };
            init_node(cx, &value, index, owner.clone())?;
            cx.instance::<Node>(&value)?.set_event_parent(cx, parent)?;
            document.nodes_push(cx, value.clone())?;
            self.publish_node(id, index);
            parent = value;
        }
        Ok(parent)
    }

    /// Cache a freshly created node id at the next handle index.
    pub(crate) fn publish_node(&mut self, id: NodeId, index: u32) {
        debug_assert_eq!(self.nodes.len(), index as usize);
        self.nodes.push(id);
        self.handles.insert(id, index);
    }
}

/// Initialize the index/owner slots shared by every node subclass instance.
pub(super) fn init_node(
    cx: &mut CallCx<'_>,
    value: &JsValue,
    index: u32,
    owner: JsValue,
) -> JsResult<()> {
    let node = cx.instance::<Node>(value)?;
    node.set_index(cx, JsValue::number(f64::from(index)))?;
    node.set_owner(cx, owner)
}

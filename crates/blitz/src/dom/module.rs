use std::cell::RefCell;
use std::rc::Rc;

use yield_binding_macros::{host_module, module_exports};
use yield_vm::{Instance, JsError, JsResult, JsValue, ModuleCx};

use super::Dom;
use super::document::{Document, DocumentSlots};
use super::element::{Element, HTMLIFrameElement, alloc_element};
use super::node::Node;
use super::resolver::init_node;
use crate::snapshot::DomState;

#[host_module(
    name = "yield:dom",
    classes = [Node, Document, Element, HTMLIFrameElement],
    services = [dom: Rc<RefCell<Dom>>],
)]
#[derive(Default, yield_vm::Encode, yield_vm::Decode)]
pub struct DomModule {
    state: DomState,
    #[js_slot]
    document: JsValue,
}

#[module_exports]
impl DomModule {
    #[constructor(name = "Node", class = Node)]
    fn construct_node(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<Node>> {
        cx.alloc(Node)
    }

    #[constructor(name = "Document", class = Document)]
    fn construct_document(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<Document>> {
        cx.alloc(Document)
    }

    #[constructor(name = "Element", class = Element)]
    fn construct_element(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<Element>> {
        alloc_element(cx, Element)
    }

    #[constructor(name = "HTMLIFrameElement", class = HTMLIFrameElement)]
    fn construct_iframe(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<HTMLIFrameElement>> {
        alloc_element(cx, HTMLIFrameElement)
    }

    #[function(name = "openDocument")]
    fn open_document(cx: &mut ModuleCx<'_, Self>, window: JsValue) -> JsResult<JsValue> {
        let cached = cx.state_slot(Self::SLOT_DOCUMENT)?;
        if !cached.is_undefined() {
            return Ok(cached);
        }
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom
            .doc
            .try_root_element()
            .and_then(|root| root.parent)
            .ok_or_else(|| JsError::fault("Blitz document has no root element parent"))?;
        let document = cx.alloc(Document)?;
        let value = document.value(cx);
        init_node(cx, &value, 0, value.clone())?;
        document.set_window(cx, window)?;
        document.nodes_push(cx, value.clone())?;
        cx.set_state_slot(Self::SLOT_DOCUMENT, value.clone())?;
        dom.publish_node(id, 0);
        Ok(value)
    }
}

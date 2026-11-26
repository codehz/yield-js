use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::{LocalName, QualName, local_name, ns};
use yield_binding_macros::{host_class, methods};
use yield_vm::binding::CallCx;
use yield_vm::{ClassCx, JsResult, JsValue, This};

use super::Dom;
use super::Node;
use super::base_node_index;

#[host_class(tag = "Document", parent = Node, methods, services = [dom: Rc<RefCell<Dom>>])]
pub(super) struct Document {
    #[js_list]
    nodes: Vec<JsValue>,
    #[js_slot]
    window: JsValue,
}

/// Validate that `this` is a bound document node and return its wrapper value.
fn bound_document(cx: &mut CallCx<'_>, this: &This<Document>) -> JsResult<JsValue> {
    let value = this.instance().value(cx);
    base_node_index(cx, &value)?;
    Ok(value)
}

#[methods(Document)]
impl Document {
    #[get(name = "defaultView")]
    fn default_view(cx: &mut CallCx<'_>, this: This<Document>) -> JsResult<JsValue> {
        this.window(cx)
    }

    #[get(name = "documentElement")]
    fn document_element(cx: &mut ClassCx<'_, '_, Self>, this: This<Document>) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.doc.try_root_element().map(|node| node.id);
        dom.resolve_node(cx, owner, id)
    }

    #[get(name = "body")]
    fn body(cx: &mut ClassCx<'_, '_, Self>, this: This<Document>) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.doc.try_root_element().and_then(|root| {
            root.children
                .iter()
                .filter_map(|id| dom.doc.get_node(*id))
                .find(|node| {
                    node.element_data()
                        .is_some_and(|el| el.name.local == local_name!("body"))
                })
                .map(|node| node.id)
        });
        dom.resolve_node(cx, owner, id)
    }

    #[get(name = "activeElement")]
    fn active_element(cx: &mut ClassCx<'_, '_, Self>, this: This<Document>) -> JsResult<JsValue> {
        Self::body(cx, this)
    }

    #[method(name = "getElementById")]
    fn get_element_by_id(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Document>,
        id: JsValue,
    ) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let id = cx.string(&id)?.to_string();
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.doc.get_element_by_id(&id);
        dom.resolve_node(cx, owner, id)
    }

    #[method(name = "createElement")]
    fn create_element(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Document>,
        tag: JsValue,
    ) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let tag = cx.string(&tag)?.to_string().to_ascii_lowercase();
        let name = QualName::new(None, ns!(html), LocalName::from(tag));
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.doc.mutate().create_element(name, Vec::new());
        dom.resolve_node(cx, owner, Some(id))
    }

    #[method(name = "createTextNode")]
    fn create_text_node(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Document>,
        value: JsValue,
    ) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let value = cx.string(&value)?.to_string();
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.doc.mutate().create_text_node(&value);
        dom.resolve_node(cx, owner, Some(id))
    }
}

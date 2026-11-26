use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::{LocalName, QualName, ns};
use yield_binding_macros::{host_class, methods};
use yield_vm::binding::CallCx;
use yield_vm::{ClassCx, HostClass, Instance, JsResult, JsValue, This};

use super::Dom;
use super::{Node, base_node_index, node_name};

#[host_class(tag = "Element", parent = Node, methods, services = [dom: Rc<RefCell<Dom>>])]
pub(super) struct Element;

#[host_class(tag = "HTMLElement", parent = Element)]
pub(super) struct HTMLElement;

#[host_class(tag = "HTMLButtonElement", parent = HTMLElement)]
pub(super) struct HTMLButtonElement;

#[host_class(tag = "HTMLLabelElement", parent = HTMLElement)]
pub(super) struct HTMLLabelElement;

#[host_class(tag = "HTMLUListElement", parent = HTMLElement)]
pub(super) struct HTMLUListElement;

#[host_class(tag = "HTMLLIElement", parent = HTMLElement)]
pub(super) struct HTMLLIElement;

#[host_class(tag = "HTMLIFrameElement", parent = HTMLElement)]
pub(super) struct HTMLIFrameElement;

#[methods(Element)]
impl Element {
    #[get(name = "tagName")]
    fn tag_name(cx: &mut ClassCx<'_, '_, Self>, this: This<Element>) -> JsResult<JsValue> {
        let value = this.instance().value(cx);
        let index = base_node_index(cx, &value)?;
        node_name(Self::dom(cx.services()), index)
    }

    #[get(name = "namespaceURI")]
    fn namespace_uri(cx: &mut ClassCx<'_, '_, Self>, this: This<Element>) -> JsResult<JsValue> {
        let value = this.instance().value(cx);
        let index = base_node_index(cx, &value)?;
        let dom = Self::dom(cx.services()).borrow();
        let name = &dom.node(index)?.element_data().unwrap().name;
        Ok(if name.ns == ns!() {
            JsValue::null()
        } else {
            JsValue::string(&*name.ns)
        })
    }

    #[method(name = "getAttribute")]
    fn get_attribute(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Element>,
        name: JsValue,
    ) -> JsResult<JsValue> {
        let node_value = this.instance().value(cx);
        let index = base_node_index(cx, &node_value)?;
        let name = LocalName::from(cx.string(&name)?.to_string().to_ascii_lowercase());
        let dom = Self::dom(cx.services()).borrow();
        Ok(dom
            .node(index)?
            .attr(name)
            .map_or_else(JsValue::null, JsValue::string))
    }

    #[method(name = "removeAttribute")]
    fn remove_attribute(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Element>,
        name: JsValue,
    ) -> JsResult<()> {
        let node_value = this.instance().value(cx);
        let index = base_node_index(cx, &node_value)?;
        let name = LocalName::from(cx.string(&name)?.to_string().to_ascii_lowercase());
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.node(index)?.id;
        if name == blitz_dom::local_name!("checked")
            && dom
                .node(index)?
                .element_data()
                .is_some_and(super::input::is_input)
        {
            dom.mutate_checked_attribute(id, None);
            return Ok(());
        }
        dom.doc
            .mutate()
            .clear_attribute(id, QualName::new(None, ns!(), name));
        Ok(())
    }

    #[method(name = "setAttribute")]
    fn set_attribute(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Element>,
        name: JsValue,
        value: JsValue,
    ) -> JsResult<()> {
        let node_value = this.instance().value(cx);
        let index = base_node_index(cx, &node_value)?;
        let name = cx.string(&name)?.to_string().to_ascii_lowercase();
        let value = cx.string(&value)?.to_string();
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.node(index)?.id;
        if name == "checked"
            && dom
                .node(index)?
                .element_data()
                .is_some_and(super::input::is_input)
        {
            dom.mutate_checked_attribute(id, Some(&value));
            return Ok(());
        }
        let name = QualName::new(None, ns!(), LocalName::from(name));
        dom.doc.mutate().set_attribute(id, name, &value);
        Ok(())
    }
}

/// Allocate an element-class instance and initialize its `style` field.
pub(super) fn alloc_element<T: HostClass>(cx: &mut CallCx<'_>, class: T) -> JsResult<Instance<T>> {
    let element = cx.alloc(class)?;
    element.init_object_field(cx, "style")?;
    Ok(element)
}

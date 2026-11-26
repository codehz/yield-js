use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::{LocalName, QualName, ns};
use yield_binding_macros::{host_class, methods};
use yield_vm::binding::CallCx;
use yield_vm::{ClassCx, HostClass, Instance, JsResult, JsValue, This};

use super::Dom;
use super::{Node, node_name};

#[host_class(tag = "Element", parent = Node, methods, services = [dom: Rc<RefCell<Dom>>])]
pub(super) struct Element;

#[host_class(tag = "HTMLElement", parent = Element)]
pub(super) struct HTMLElement;

macro_rules! declare_html_element {
    (plain $class:ident, $name:tt) => {
        #[host_class(tag = $name, parent = HTMLElement)]
        pub(super) struct $class;
    };
    (custom $class:ident, $name:tt) => {
        pub(super) use super::input::$class;
    };
}

macro_rules! bind_html_elements {
    (
        { $(($tag:literal, $class:ident, $name:tt, $ctor:ident, $mode:ident),)* }
    ) => {
        $(declare_html_element!($mode $class, $name);)*

        pub(super) fn alloc_html_element(cx: &mut CallCx<'_>, tag: &str) -> JsResult<JsValue> {
            Ok(match tag {
                $($tag => alloc_element(cx, $class)?.value(cx),)*
                _ => alloc_element(cx, HTMLElement)?.value(cx),
            })
        }
    };
}

html_elements!(bind_html_elements);

#[methods(Element)]
impl Element {
    #[get(name = "tagName")]
    fn tag_name(cx: &mut ClassCx<'_, '_, Self>, this: This<Element>) -> JsResult<JsValue> {
        let index = receiver_index!(cx, this)?;
        node_name(Self::dom(cx.services()), index)
    }

    #[get(name = "namespaceURI")]
    fn namespace_uri(cx: &mut ClassCx<'_, '_, Self>, this: This<Element>) -> JsResult<JsValue> {
        let index = receiver_index!(cx, this)?;
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
        let index = receiver_index!(cx, this)?;
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
        let index = receiver_index!(cx, this)?;
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
        let index = receiver_index!(cx, this)?;
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

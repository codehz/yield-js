use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::{QualName, local_name, ns};
use yield_binding_macros::{host_module, module_exports};
use yield_vm::intrinsics::TypeError;
use yield_vm::{HostStep, Instance, JsError, JsResult, JsValue, ModuleCx};

use super::Dom;
use super::document::{Document, DocumentSlots};
use super::element::{Element, HTMLElement, alloc_element};
use super::node::Node;
use super::resolver::init_node;
use crate::snapshot::DomState;

macro_rules! define_dom_module {
    (
        { $(($tag:literal, $class:ident, $name:tt, $ctor:ident, $mode:ident),)* }
        $($item:item)*
    ) => {
        use super::element::{$($class),*};

        #[host_module(
            name = "yield:dom",
            classes = [Node, Document, Element, HTMLElement, $($class,)*],
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

            #[constructor(name = "HTMLElement", class = HTMLElement)]
            fn construct_html_element(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<HTMLElement>> {
                alloc_element(cx, HTMLElement)
            }

            $(
                #[constructor(name = $name, class = $class)]
                fn $ctor(cx: &mut ModuleCx<'_, Self>) -> JsResult<Instance<$class>> {
                    alloc_element(cx, $class)
                }
            )*

            $($item)*
        }
    };
}

html_elements! {
    define_dom_module

    #[function(name = "dispatchUserEvent")]
    async fn dispatch_user_event(cx: &mut ModuleCx<'_, Self>) -> JsResult<HostStep> {
        let document = cx.state_slot(Self::SLOT_DOCUMENT)?;
        let (kind, target) = {
            let mut dom = Self::dom(cx.services()).borrow_mut();
            let (kind, id) = dom.pending_event.take().ok_or_else(|| {
                JsError::new::<TypeError>("dispatchUserEvent requires input from the host")
            })?;
            (kind, dom.resolve_node(cx, document, Some(id))?)
        };
        let event = cx.event(kind.name(), true, kind.cancelable())?;
        let accepted = cx.dispatch_event(target, event).await?;
        Ok(HostStep::Return(accepted))
    }

    #[function(name = "insertCss")]
    fn insert_css(cx: &mut ModuleCx<'_, Self>, uri: JsValue) -> JsResult<()> {
        let uri = cx.string(&uri)?.to_utf8().map_err(|_| {
            JsError::new::<TypeError>("unknown CSS resource: URI is not valid Unicode")
        })?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let Dom { doc, resources, .. } = &mut *dom;
        let bytes = resources
            .get(&uri)
            .ok_or_else(|| JsError::new::<TypeError>(format!("unknown CSS resource: {uri}")))?;
        let css = std::str::from_utf8(bytes)
            .map_err(|_| JsError::new::<TypeError>(format!("invalid UTF-8 CSS resource: {uri}")))?;
        let head = doc
            .try_root_element()
            .into_iter()
            .flat_map(|root| &root.children)
            .filter_map(|&id| doc.get_node(id))
            .find(|node| {
                node.element_data().is_some_and(|element| {
                    element.name.ns == ns!(html) && element.name.local == local_name!("head")
                })
            })
            .ok_or_else(|| JsError::new::<TypeError>("Blitz document has no head element"))?
            .id;
        let mut mutation = doc.mutate();
        let style = mutation.create_element(
            QualName::new(None, ns!(html), local_name!("style")),
            Vec::new(),
        );
        let text = mutation.create_text_node(css);
        mutation.append_children(style, &[text]);
        mutation.append_children(head, &[style]);
        Ok(())
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

use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::NodeData;
use yield_binding_macros::{host_class, methods};
use yield_vm::binding::CallCx;
use yield_vm::{ClassCx, JsError, JsResult, JsValue, This};

use super::Dom;
use super::node_index;

#[host_class(tag = "Node", event_parent = event_parent, methods, services = [dom: Rc<RefCell<Dom>>])]
pub(crate) struct Node {
    #[js_slot]
    index: JsValue,
    #[js_slot]
    owner: JsValue,
    #[js_slot]
    event_parent: JsValue,
}

#[methods(Node)]
impl Node {
    #[get(name = "nodeType")]
    fn node_type(cx: &mut ClassCx<'_, '_, Self>, this: This<Node>) -> JsResult<JsValue> {
        let index = node_index(cx, this.instance())?;
        let dom = Self::dom(cx.services()).borrow();
        let node = dom.node(index)?;
        Ok(JsValue::number(match node.data {
            NodeData::Element(_) => 1.0,
            NodeData::Text(_) => 3.0,
            NodeData::Comment { .. } => 8.0,
            NodeData::Document(_) => 9.0,
            _ => return Err(JsError::type_error("unsupported DOM node kind")),
        }))
    }

    #[get(name = "nodeName")]
    fn node_name(cx: &mut ClassCx<'_, '_, Self>, this: This<Node>) -> JsResult<JsValue> {
        let index = node_index(cx, this.instance())?;
        super::node_name(Self::dom(cx.services()), index)
    }

    #[get(name = "firstChild")]
    fn first_child(cx: &mut ClassCx<'_, '_, Self>, this: This<Node>) -> JsResult<JsValue> {
        let owner = this.owner(cx)?;
        let index = node_index(cx, this.instance())?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.node(index)?.children.first().copied();
        dom.resolve_node(cx, owner, id)
    }

    #[get(name = "nextSibling")]
    fn next_sibling(cx: &mut ClassCx<'_, '_, Self>, this: This<Node>) -> JsResult<JsValue> {
        let owner = this.owner(cx)?;
        let index = node_index(cx, this.instance())?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let node = dom.node(index)?;
        let id = node
            .parent
            .and_then(|id| dom.doc.get_node(id))
            .and_then(|parent| {
                parent
                    .index_of_child(node.id)
                    .and_then(|position| parent.children.get(position + 1))
                    .copied()
            });
        dom.resolve_node(cx, owner, id)
    }

    #[get(name = "parentNode")]
    fn parent_node(cx: &mut ClassCx<'_, '_, Self>, this: This<Node>) -> JsResult<JsValue> {
        let owner = this.owner(cx)?;
        let index = node_index(cx, this.instance())?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.node(index)?.parent;
        dom.resolve_node(cx, owner, id)
    }

    #[get(name = "ownerDocument")]
    fn owner_document(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        let owner = this.owner(cx)?;
        Ok(if cx.same_value(&owner, &this.instance().value(cx)) {
            JsValue::null()
        } else {
            owner
        })
    }

    #[get(name = "textContent")]
    fn text_content(cx: &mut ClassCx<'_, '_, Self>, this: This<Node>) -> JsResult<JsValue> {
        let index = node_index(cx, this.instance())?;
        let dom = Self::dom(cx.services()).borrow();
        let node = dom.node(index)?;
        Ok(match &node.data {
            NodeData::Document(_) => JsValue::null(),
            NodeData::Comment { contents } => JsValue::string(contents.as_str()),
            _ => JsValue::string(node.text_content()),
        })
    }

    #[set(name = "textContent")]
    fn set_text_content(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Node>,
        value: JsValue,
    ) -> JsResult<()> {
        let index = node_index(cx, this.instance())?;
        if index == 0 {
            return Ok(());
        }
        let value = cx.string(&value)?.to_string();
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let node = dom.node(index)?;
        let id = node.id;
        let is_text = matches!(node.data, NodeData::Text(_) | NodeData::Comment { .. });
        if !is_text && !matches!(node.data, NodeData::Element(_)) {
            return Err(JsError::type_error(
                "textContent requires an element or character data node",
            ));
        }
        if !is_text {
            use super::document::{Document, DocumentSlots};
            let owner = this.owner(cx)?;
            let document = cx.instance::<Document>(&owner)?;
            for child in &node.children {
                if let Some(&index) = dom.handles.get(child) {
                    let wrapper = document.nodes_get(cx, index as usize)?;
                    cx.instance::<Node>(&wrapper)?
                        .set_event_parent(cx, JsValue::null())?;
                }
            }
        }
        let mut mutation = dom.doc.mutate();
        if is_text {
            mutation.set_node_text(id, &value);
        } else if value.is_empty() {
            mutation.replace_children(id, &[]);
        } else {
            let text = mutation.create_text_node(&value);
            mutation.replace_children(id, &[text]);
        }
        Ok(())
    }

    #[method(name = "appendChild")]
    fn append_child(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Node>,
        child: JsValue,
    ) -> JsResult<JsValue> {
        let child_node = cx.instance::<Node>(&child)?;
        let owner = this.owner(cx)?;
        let child_owner = child_node.owner(cx)?;
        if !cx.same_value(&owner, &child_owner) {
            return Err(cx.type_error("appendChild requires nodes from the same document"));
        }
        let parent_index = node_index(cx, this.instance())?;
        let child_index = node_index(cx, child_node)?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let parent = dom.node(parent_index)?;
        let child_node = dom.node(child_index)?;
        if !matches!(parent.data, NodeData::Element(_)) {
            return Err(JsError::type_error("append requires an element parent"));
        }
        if !matches!(child_node.data, NodeData::Element(_) | NodeData::Text(_)) {
            return Err(JsError::type_error(
                "append requires an element or text child",
            ));
        }
        let mut ancestor = Some(parent);
        while let Some(node) = ancestor {
            if node.id == child_node.id {
                return Err(JsError::type_error(
                    "cannot insert a node into itself or its descendants",
                ));
            }
            ancestor = node.parent.and_then(|id| dom.doc.get_node(id));
        }
        let (parent, child_id) = (parent.id, child_node.id);
        let parent_wrapper = this.instance().value(cx);
        cx.instance::<Node>(&child)?
            .set_event_parent(cx, parent_wrapper)?;
        dom.doc.mutate().append_children(parent, &[child_id]);
        Ok(child)
    }
}

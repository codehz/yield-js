use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::NodeData;
use yield_binding_macros::{host_class, methods};
use yield_vm::binding::{CallCx, with_class};
use yield_vm::intrinsics::TypeError;
use yield_vm::{ClassCx, JsError, JsResult, JsValue, This};

use super::Dom;
use super::node_index;

#[host_class(tag = "Node", event_parent_fn = event_parent, methods, services = [dom: Rc<RefCell<Dom>>])]
pub(crate) struct Node {
    #[js_slot]
    index: JsValue,
    #[js_slot]
    owner: JsValue,
}

fn event_parent(cx: &mut CallCx<'_>, receiver: JsValue) -> JsResult<JsValue> {
    with_class::<Node, _>(cx, |cx| {
        let wrapper = cx.instance::<Node>(&receiver)?;
        let index = node_index(cx, wrapper)?;
        let owner = wrapper.owner(cx)?;
        let mut dom = Node::dom(cx.services()).borrow_mut();
        let parent = dom.node(index)?.parent;
        dom.resolve_node(cx, owner, parent)
    })
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
            _ => return Err(JsError::new::<TypeError>("unsupported DOM node kind")),
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
            return Err(JsError::new::<TypeError>(
                "textContent requires an element or character data node",
            ));
        }
        let Dom {
            doc,
            handles,
            detached,
            dirty_checkboxes,
            ..
        } = &mut *dom;
        if is_text {
            doc.mutate().set_node_text(id, &value);
        } else {
            // Branches may contain exposed descendants even when their roots
            // were never wrapped. Defer those to component sweep.
            let unwrapped_leaves: Vec<_> = doc
                .get_node(id)
                .expect("validated DOM node")
                .children
                .iter()
                .copied()
                .filter(|child| {
                    if handles.contains_key(child)
                        || !doc
                            .get_node(*child)
                            .expect("live DOM child")
                            .children
                            .is_empty()
                    {
                        detached.insert(*child);
                        false
                    } else {
                        true
                    }
                })
                .collect();
            let mut mutation = doc.mutate();
            if value.is_empty() {
                mutation.replace_children(id, &[]);
            } else {
                let text = mutation.create_text_node(&value);
                mutation.replace_children(id, &[text]);
            }
            for child in unwrapped_leaves {
                mutation.remove_node_if_unparented_with(child, &mut |id| {
                    dirty_checkboxes.remove(&id);
                });
            }
        }
        Ok(())
    }

    #[method(name = "appendChild")]
    fn append_child(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Node>,
        child: JsValue,
    ) -> JsResult<JsValue> {
        Self::mutate_child(
            cx,
            this,
            child,
            ChildMutation::InsertBefore(JsValue::null()),
        )
    }

    #[method(name = "insertBefore")]
    fn insert_before(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Node>,
        child: JsValue,
        reference: JsValue,
    ) -> JsResult<JsValue> {
        Self::mutate_child(cx, this, child, ChildMutation::InsertBefore(reference))
    }

    #[method(name = "removeChild")]
    fn remove_child(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Node>,
        child: JsValue,
    ) -> JsResult<JsValue> {
        Self::mutate_child(cx, this, child, ChildMutation::Remove)
    }
}

enum ChildMutation {
    InsertBefore(JsValue),
    Remove,
}

impl Node {
    fn mutate_child(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Node>,
        child: JsValue,
        operation: ChildMutation,
    ) -> JsResult<JsValue> {
        let owner = this.owner(cx)?;
        let same_document_node = |cx: &mut CallCx<'_>, value: &JsValue| {
            let node = cx.instance::<Node>(value)?;
            let node_owner = node.owner(cx)?;
            if !cx.same_value(&owner, &node_owner) {
                return Err(JsError::new::<TypeError>(
                    "child mutation requires nodes from the same document",
                ));
            }
            node_index(cx, node)
        };
        let child_index = same_document_node(cx, &child)?;
        let parent_index = node_index(cx, this.instance())?;
        let reference = match &operation {
            ChildMutation::InsertBefore(reference) if !reference.is_null() => {
                Some(same_document_node(cx, reference)?)
            }
            _ => None,
        };
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let parent = dom.node(parent_index)?;
        let child_node = dom.node(child_index)?;
        let (parent_id, child_id) = (parent.id, child_node.id);
        if matches!(operation, ChildMutation::Remove) {
            if child_node.parent != Some(parent_id) {
                return Err(JsError::new::<TypeError>("node is not a direct child"));
            }
            dom.doc.mutate().remove_node(child_id);
            dom.detached.insert(child_id);
            return Ok(child);
        }
        if !matches!(parent.data, NodeData::Element(_)) {
            return Err(JsError::new::<TypeError>(
                "insert requires an element parent",
            ));
        }
        if !matches!(child_node.data, NodeData::Element(_) | NodeData::Text(_)) {
            return Err(JsError::new::<TypeError>(
                "insert requires an element or text child",
            ));
        }
        let reference = reference
            .map(|index| {
                let node = dom.node(index)?;
                if node.parent != Some(parent_id) {
                    return Err(JsError::new::<TypeError>("reference is not a direct child"));
                }
                Ok(node.id)
            })
            .transpose()?;
        if reference == Some(child_id) {
            return Ok(child);
        }
        let mut ancestor = Some(parent);
        while let Some(node) = ancestor {
            if node.id == child_id {
                return Err(JsError::new::<TypeError>(
                    "cannot insert a node into itself or its descendants",
                ));
            }
            ancestor = node.parent.and_then(|id| dom.doc.get_node(id));
        }
        let mut mutation = dom.doc.mutate();
        if let Some(reference) = reference {
            mutation.insert_nodes_before(reference, &[child_id]);
        } else {
            mutation.append_children(parent_id, &[child_id]);
        }
        drop(mutation);
        dom.detached.remove(&child_id);
        Ok(child)
    }
}

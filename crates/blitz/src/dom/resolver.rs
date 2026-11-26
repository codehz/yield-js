//! Shared `Dom` helpers: id→handle resolution and wrapper initialization.

use std::collections::{BTreeMap, BTreeSet};

use blitz_dom::{NodeData, NodeId, ns};
use yield_vm::binding::CallCx;
use yield_vm::intrinsics::TypeError;
use yield_vm::{Instance, JsError, JsResult, JsValue, ObjectId};

use super::Dom;
use super::element::{Element, alloc_element, alloc_html_element};
use super::node::{Node, NodeSlots};

impl Dom {
    /// Resolve a handle index to the Blitz node it wraps.
    pub(crate) fn node(&self, index: u32) -> JsResult<&blitz_dom::Node> {
        self.nodes
            .get(&index)
            .and_then(|&id| self.doc.get_node(id))
            .ok_or_else(|| JsError::new::<TypeError>("invalid DOM node handle"))
    }

    pub(super) fn wrapper(
        &self,
        cx: &mut CallCx<'_>,
        id: NodeId,
    ) -> JsResult<Option<Instance<Node>>> {
        self.handles
            .get(&id)
            .map(|identity| {
                let object =
                    identity.ok_or_else(|| JsError::fault("missing DOM wrapper identity"))?;
                cx.instance_by_identity(object)
            })
            .transpose()
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
        if let Some(node) = self.wrapper(cx, id)? {
            return Ok(node.value(cx));
        }

        let index = self.next_handle;
        if index == u32::MAX {
            return Err(JsError::new::<TypeError>("too many DOM nodes"));
        }
        let node = self
            .doc
            .get_node(id)
            .ok_or_else(|| JsError::new::<TypeError>("invalid DOM node handle"))?;
        let value = match &node.data {
            NodeData::Element(element) if element.name.ns == ns!(html) => {
                alloc_html_element(cx, element.name.local.as_ref())?
            }
            NodeData::Element(_) => alloc_element(cx, Element)?.value(cx),
            NodeData::Text(_) | NodeData::Comment { .. } => cx.alloc(Node)?.value(cx),
            _ => return Err(JsError::new::<TypeError>("unsupported DOM node kind")),
        };
        init_node(cx, &value, index, owner)?;
        let wrapper = cx.instance::<Node>(&value)?;
        self.publish_node(id, index, wrapper.identity() as ObjectId);
        Ok(value)
    }

    /// Cache a freshly created node id at the next handle index.
    pub(crate) fn publish_node(&mut self, id: NodeId, index: u32, object: ObjectId) {
        debug_assert_eq!(self.next_handle, index);
        self.next_handle += 1;
        self.nodes.insert(index, id);
        self.handles.insert(id, Some(object));
    }

    pub(crate) fn wrapper_groups(&self) -> Result<Vec<Vec<ObjectId>>, String> {
        Ok(self.wrappers_by_root()?.into_values().collect())
    }

    /// Group exposed wrappers by native component, caching shared ancestor paths
    /// only for this trace. An unwrapped root can still own a live component.
    pub(crate) fn wrappers_by_root(&self) -> Result<BTreeMap<NodeId, Vec<ObjectId>>, String> {
        let mut groups = BTreeMap::<_, Vec<_>>::new();
        let mut roots = BTreeMap::new();
        let mut path = Vec::new();
        for (&id, &identity) in &self.handles {
            let object = identity.ok_or("missing DOM wrapper identity")?;
            let mut current = id;
            let root = loop {
                if let Some(&root) = roots.get(&current) {
                    break root;
                }
                let node = self.doc.get_node(current).ok_or("missing live DOM node")?;
                path.push(current);
                match node.parent {
                    Some(parent) => current = parent,
                    None => break current,
                }
            };
            roots.extend(path.drain(..).map(|id| (id, root)));
            groups.entry(root).or_default().push(object);
        }
        Ok(groups)
    }

    pub(crate) fn sweep(
        &mut self,
        live_roots: &BTreeSet<NodeId>,
        is_live: impl Fn(ObjectId) -> bool,
    ) {
        let handles = &mut self.handles;
        self.nodes.retain(|_, id| {
            let live = handles[id].is_some_and(&is_live);
            if !live {
                handles.remove(id);
            }
            live
        });
        let Self {
            doc,
            dirty_checkboxes,
            detached,
            ..
        } = self;
        let mut mutation = doc.mutate();
        detached.retain(|id| {
            if live_roots.contains(id) {
                return true;
            }
            mutation.remove_node_if_unparented_with(*id, &mut |id| {
                dirty_checkboxes.remove(&id);
            });
            false
        });
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

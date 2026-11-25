use blitz_dom::{LocalName, Node, NodeData, NodeId, QualName, local_name, ns};
use yield_binding_macros::{host_module, module_exports};
use yield_vm::{JsError, JsResult, JsValue, ModuleCx};

use super::{ACTIVE_DOM, Dom};

type Cx<'a> = ModuleCx<'a, DomModule>;
type DomResult<T> = Result<T, &'static str>;

impl Dom {
    fn node(&self, handle: f64) -> DomResult<&Node> {
        let index = handle as u32;
        if f64::from(index) != handle {
            return Err("invalid DOM node handle");
        }
        self.nodes
            .get(index as usize)
            .and_then(|id| self.doc.get_node(*id))
            .ok_or("invalid DOM node handle")
    }

    fn handle(&mut self, id: NodeId) -> DomResult<JsValue> {
        let index = if let Some(index) = self.handles.get(&id) {
            *index
        } else {
            let index = u32::try_from(self.nodes.len()).map_err(|_| "too many DOM nodes")?;
            self.nodes.push(id);
            self.handles.insert(id, index);
            index
        };
        Ok(JsValue::number(f64::from(index)))
    }

    fn optional_handle(&mut self, id: Option<NodeId>) -> DomResult<JsValue> {
        id.map_or(Ok(JsValue::null()), |id| self.handle(id))
    }
}

fn with_dom<T>(cx: &mut Cx<'_>, f: impl FnOnce(&mut Dom) -> DomResult<T>) -> JsResult<T> {
    if !ACTIVE_DOM.is_set() {
        return Err(JsError::Fault(cx.fault("no active Blitz document")));
    }
    ACTIVE_DOM
        .with(|dom| f(&mut dom.borrow_mut()))
        .map_err(|error| cx.type_error(error))
}

#[host_module(name = "yield:dom")]
pub(super) struct DomModule;

#[module_exports]
impl DomModule {
    #[function(name = "byId")]
    fn by_id(cx: &mut ModuleCx<'_, Self>, id: JsValue) -> JsResult<JsValue> {
        let id = cx.string(&id)?.to_string();
        with_dom(cx, |dom| {
            dom.optional_handle(dom.doc.get_element_by_id(&id))
        })
    }

    #[function(name = "documentElement")]
    fn document_element(cx: &mut ModuleCx<'_, Self>) -> JsResult<JsValue> {
        with_dom(cx, |dom| {
            dom.optional_handle(dom.doc.try_root_element().map(|node| node.id))
        })
    }

    #[function(name = "body")]
    fn body(cx: &mut ModuleCx<'_, Self>) -> JsResult<JsValue> {
        with_dom(cx, |dom| {
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
            dom.optional_handle(id)
        })
    }

    #[function(name = "element")]
    fn element(cx: &mut ModuleCx<'_, Self>, tag: JsValue) -> JsResult<JsValue> {
        let tag = cx.string(&tag)?.to_string().to_ascii_lowercase();
        with_dom(cx, |dom| {
            let name = QualName::new(None, ns!(html), LocalName::from(tag));
            let id = dom.doc.mutate().create_element(name, Vec::new());
            dom.handle(id)
        })
    }

    #[function(name = "text")]
    fn text(cx: &mut ModuleCx<'_, Self>, value: JsValue) -> JsResult<JsValue> {
        let value = cx.string(&value)?.to_string();
        with_dom(cx, |dom| {
            let id = dom.doc.mutate().create_text_node(&value);
            dom.handle(id)
        })
    }

    #[function(name = "append")]
    fn append(cx: &mut ModuleCx<'_, Self>, parent: JsValue, child: JsValue) -> JsResult<()> {
        let parent = cx.number(&parent)?;
        let child = cx.number(&child)?;
        with_dom(cx, |dom| {
            let parent = dom.node(parent)?;
            let child = dom.node(child)?;
            if !matches!(parent.data, NodeData::Element(_)) {
                return Err("append requires an element parent");
            }
            if !matches!(child.data, NodeData::Element(_) | NodeData::Text(_)) {
                return Err("append requires an element or text child");
            }
            let mut ancestor = Some(parent);
            while let Some(node) = ancestor {
                if node.id == child.id {
                    return Err("cannot insert a node into itself or its descendants");
                }
                ancestor = node.parent.and_then(|id| dom.doc.get_node(id));
            }
            let (parent, child) = (parent.id, child.id);
            dom.doc.mutate().append_children(parent, &[child]);
            Ok(())
        })
    }

    #[function(name = "setText")]
    fn set_text(cx: &mut ModuleCx<'_, Self>, node: JsValue, value: JsValue) -> JsResult<()> {
        let handle = cx.number(&node)?;
        let value = cx.string(&value)?.to_string();
        with_dom(cx, |dom| {
            let node = dom.node(handle)?;
            let id = node.id;
            let is_text = matches!(node.data, NodeData::Text(_) | NodeData::Comment { .. });
            if !is_text && !matches!(node.data, NodeData::Element(_)) {
                return Err("textContent requires an element or character data node");
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
        })
    }

    #[function(name = "setAttr")]
    fn set_attr(
        cx: &mut ModuleCx<'_, Self>,
        node: JsValue,
        name: JsValue,
        value: JsValue,
    ) -> JsResult<()> {
        let handle = cx.number(&node)?;
        let name = cx.string(&name)?.to_string().to_ascii_lowercase();
        let value = cx.string(&value)?.to_string();
        with_dom(cx, |dom| {
            let node = dom.node(handle)?;
            if !matches!(node.data, NodeData::Element(_)) {
                return Err("setAttribute requires an element");
            }
            let id = node.id;
            let name = QualName::new(None, ns!(), LocalName::from(name));
            dom.doc.mutate().set_attribute(id, name, &value);
            Ok(())
        })
    }

    #[function(name = "first")]
    fn first(cx: &mut ModuleCx<'_, Self>, node: JsValue) -> JsResult<JsValue> {
        let handle = cx.number(&node)?;
        with_dom(cx, |dom| {
            dom.optional_handle(dom.node(handle)?.children.first().copied())
        })
    }

    #[function(name = "next")]
    fn next(cx: &mut ModuleCx<'_, Self>, node: JsValue) -> JsResult<JsValue> {
        let handle = cx.number(&node)?;
        with_dom(cx, |dom| {
            let node = dom.node(handle)?;
            let id = node
                .parent
                .and_then(|id| dom.doc.get_node(id))
                .and_then(|parent| {
                    parent
                        .index_of_child(node.id)
                        .and_then(|index| parent.children.get(index + 1).copied())
                });
            dom.optional_handle(id)
        })
    }

    #[function(name = "parent")]
    fn parent(cx: &mut ModuleCx<'_, Self>, node: JsValue) -> JsResult<JsValue> {
        let handle = cx.number(&node)?;
        with_dom(cx, |dom| dom.optional_handle(dom.node(handle)?.parent))
    }

    #[function(name = "kind")]
    fn kind(cx: &mut ModuleCx<'_, Self>, node: JsValue) -> JsResult<JsValue> {
        let handle = cx.number(&node)?;
        with_dom(cx, |dom| {
            Ok(JsValue::number(f64::from(match dom.node(handle)?.data {
                NodeData::Element(_) => 1,
                NodeData::Text(_) => 3,
                NodeData::Comment { .. } => 8,
                NodeData::Document(_) => 9,
                _ => return Err("unsupported DOM node kind"),
            })))
        })
    }

    #[function(name = "name")]
    fn name(cx: &mut ModuleCx<'_, Self>, node: JsValue) -> JsResult<JsValue> {
        let handle = cx.number(&node)?;
        with_dom(cx, |dom| {
            Ok(JsValue::string(match &dom.node(handle)?.data {
                NodeData::Element(element) => element.name.local.to_string(),
                NodeData::Text(_) => "#text".to_owned(),
                NodeData::Comment { .. } => "#comment".to_owned(),
                NodeData::Document(_) => "#document".to_owned(),
                _ => return Err("unsupported DOM node kind"),
            }))
        })
    }

    #[function(name = "readText")]
    fn read_text(cx: &mut ModuleCx<'_, Self>, node: JsValue) -> JsResult<JsValue> {
        let handle = cx.number(&node)?;
        with_dom(cx, |dom| {
            let node = dom.node(handle)?;
            Ok(match &node.data {
                NodeData::Document(_) => JsValue::null(),
                NodeData::Comment { contents } => JsValue::string(contents.as_str()),
                _ => JsValue::string(node.text_content()),
            })
        })
    }
}

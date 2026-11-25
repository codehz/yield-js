use std::cell::{Ref, RefMut};

use blitz_dom::{LocalName, Node as BlitzNode, NodeData, NodeId, QualName, local_name, ns};
use yield_binding_macros::{host_class, host_module, methods, module_exports};
use yield_vm::binding::CallCx;
use yield_vm::{Instance, JsError, JsResult, JsValue, ModuleCx, This};

use super::{Dom, DomKey};

impl Dom {
    fn node(&self, index: u32) -> JsResult<&BlitzNode> {
        self.nodes
            .get(index as usize)
            .and_then(|id| self.doc.get_node(*id))
            .ok_or_else(|| JsError::type_error("invalid DOM node handle"))
    }
}

fn dom_ref<'a>(cx: &'a mut CallCx<'_>) -> JsResult<Ref<'a, Dom>> {
    let service = cx
        .service::<DomKey>()
        .ok_or_else(|| JsError::fault("no active Blitz document"))?;
    Ok(service.borrow())
}

fn dom_mut<'a>(cx: &'a mut CallCx<'_>) -> JsResult<RefMut<'a, Dom>> {
    let service = cx
        .service::<DomKey>()
        .ok_or_else(|| JsError::fault("no active Blitz document"))?;
    Ok(service.borrow_mut())
}

#[host_class(tag = "Node", event_target, methods)]
struct Node {
    #[js_slot]
    index: JsValue,
    #[js_slot]
    owner: JsValue,
}

#[host_class(tag = "Document", parent = Node, methods)]
struct Document {
    #[js_list]
    nodes: Vec<JsValue>,
    #[js_slot]
    window: JsValue,
}

#[host_class(tag = "Element", parent = Node, methods)]
struct Element;

fn node_index(cx: &mut CallCx<'_>, node: Instance<Node>) -> JsResult<u32> {
    node.index(cx)?
        .as_number()
        .map(|index| index as u32)
        .ok_or_else(|| cx.type_error("DOM node is not bound to a document"))
}

#[methods(Node)]
impl Node {
    #[get(name = "nodeType")]
    fn node_type(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        let index = node_index(cx, this.instance())?;
        let dom = dom_ref(cx)?;
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
    fn node_name(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        node_name(cx, this.instance())
    }

    #[get(name = "firstChild")]
    fn first_child(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        let owner = this.owner(cx)?;
        let index = node_index(cx, this.instance())?;
        let dom = dom_ref(cx)?;
        let id = dom.node(index)?.children.first().copied();
        drop(dom);
        resolve_node(cx, owner, id)
    }

    #[get(name = "nextSibling")]
    fn next_sibling(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        let owner = this.owner(cx)?;
        let index = node_index(cx, this.instance())?;
        let dom = dom_ref(cx)?;
        let node = dom.node(index)?;
        let parent = node.parent.and_then(|id| dom.doc.get_node(id));
        let id = parent.and_then(|parent| {
            parent
                .index_of_child(node.id)
                .and_then(|position| parent.children.get(position + 1))
                .copied()
        });
        drop(dom);
        resolve_node(cx, owner, id)
    }

    #[get(name = "parentNode")]
    fn parent_node(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        let owner = this.owner(cx)?;
        let index = node_index(cx, this.instance())?;
        let dom = dom_ref(cx)?;
        let id = dom.node(index)?.parent;
        drop(dom);
        resolve_node(cx, owner, id)
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
    fn text_content(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        let index = node_index(cx, this.instance())?;
        let dom = dom_ref(cx)?;
        let node = dom.node(index)?;
        Ok(match &node.data {
            NodeData::Document(_) => JsValue::null(),
            NodeData::Comment { contents } => JsValue::string(contents.as_str()),
            _ => JsValue::string(node.text_content()),
        })
    }

    #[set(name = "textContent")]
    fn set_text_content(cx: &mut CallCx<'_>, this: This<Node>, value: JsValue) -> JsResult<()> {
        let index = node_index(cx, this.instance())?;
        if index == 0 {
            return Ok(());
        }
        let value = cx.string(&value)?.to_string();
        let mut dom = dom_mut(cx)?;
        let node = dom.node(index)?;
        let id = node.id;
        let is_text = matches!(node.data, NodeData::Text(_) | NodeData::Comment { .. });
        if !is_text && !matches!(node.data, NodeData::Element(_)) {
            return Err(JsError::type_error(
                "textContent requires an element or character data node",
            ));
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
    fn append_child(cx: &mut CallCx<'_>, this: This<Node>, child: JsValue) -> JsResult<JsValue> {
        let child_node = cx.instance::<Node>(&child)?;
        if !cx.same_value(&this.owner(cx)?, &child_node.owner(cx)?) {
            return Err(cx.type_error("appendChild requires nodes from the same document"));
        }
        let parent_index = node_index(cx, this.instance())?;
        let child_index = node_index(cx, child_node)?;
        let child_value = child;
        let mut dom = dom_mut(cx)?;
        let parent = dom.node(parent_index)?;
        let child = dom.node(child_index)?;
        if !matches!(parent.data, NodeData::Element(_)) {
            return Err(JsError::type_error("append requires an element parent"));
        }
        if !matches!(child.data, NodeData::Element(_) | NodeData::Text(_)) {
            return Err(JsError::type_error(
                "append requires an element or text child",
            ));
        }
        let mut ancestor = Some(parent);
        while let Some(node) = ancestor {
            if node.id == child.id {
                return Err(JsError::type_error(
                    "cannot insert a node into itself or its descendants",
                ));
            }
            ancestor = node.parent.and_then(|id| dom.doc.get_node(id));
        }
        let (parent, child) = (parent.id, child.id);
        dom.doc.mutate().append_children(parent, &[child]);
        Ok(child_value)
    }
}

fn node_name(cx: &mut CallCx<'_>, node: Instance<Node>) -> JsResult<JsValue> {
    let index = node_index(cx, node)?;
    let dom = dom_ref(cx)?;
    let node = dom.node(index)?;
    Ok(match &node.data {
        NodeData::Element(element) => JsValue::string(element.name.local.to_uppercase()),
        NodeData::Text(_) => JsValue::string("#text"),
        NodeData::Comment { .. } => JsValue::string("#comment"),
        NodeData::Document(_) => JsValue::string("#document"),
        _ => return Err(JsError::type_error("unsupported DOM node kind")),
    })
}

#[methods(Element)]
impl Element {
    #[get(name = "tagName")]
    fn tag_name(cx: &mut CallCx<'_>, this: This<Element>) -> JsResult<JsValue> {
        let node = cx.instance::<Node>(&this.instance().value(cx))?;
        node_name(cx, node)
    }

    #[get(name = "namespaceURI")]
    fn namespace_uri(_cx: &mut CallCx<'_>, _this: This<Element>) -> JsResult<JsValue> {
        Ok(JsValue::string("http://www.w3.org/1999/xhtml"))
    }

    #[method(name = "setAttribute")]
    fn set_attribute(
        cx: &mut CallCx<'_>,
        this: This<Element>,
        name: JsValue,
        value: JsValue,
    ) -> JsResult<()> {
        let node = cx.instance::<Node>(&this.instance().value(cx))?;
        let index = node_index(cx, node)?;
        let name = cx.string(&name)?.to_string().to_ascii_lowercase();
        let value = cx.string(&value)?.to_string();
        let mut dom = dom_mut(cx)?;
        let node = dom.node(index)?;
        let id = node.id;
        let name = QualName::new(None, ns!(), LocalName::from(name));
        dom.doc.mutate().set_attribute(id, name, &value);
        Ok(())
    }
}

fn bound_document(cx: &mut CallCx<'_>, this: &This<Document>) -> JsResult<JsValue> {
    let value = this.instance().value(cx);
    let node = cx.instance::<Node>(&value)?;
    node_index(cx, node)?;
    Ok(value)
}

#[methods(Document)]
impl Document {
    #[get(name = "defaultView")]
    fn default_view(cx: &mut CallCx<'_>, this: This<Document>) -> JsResult<JsValue> {
        this.window(cx)
    }

    #[get(name = "documentElement")]
    fn document_element(cx: &mut CallCx<'_>, this: This<Document>) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let dom = dom_ref(cx)?;
        let id = dom.doc.try_root_element().map(|node| node.id);
        drop(dom);
        resolve_node(cx, owner, id)
    }

    #[get(name = "body")]
    fn body(cx: &mut CallCx<'_>, this: This<Document>) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let dom = dom_ref(cx)?;
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
        drop(dom);
        resolve_node(cx, owner, id)
    }

    #[get(name = "activeElement")]
    fn active_element(cx: &mut CallCx<'_>, this: This<Document>) -> JsResult<JsValue> {
        Self::body(cx, this)
    }

    #[method(name = "getElementById")]
    fn get_element_by_id(
        cx: &mut CallCx<'_>,
        this: This<Document>,
        id: JsValue,
    ) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let id = cx.string(&id)?.to_string();
        let dom = dom_ref(cx)?;
        let id = dom.doc.get_element_by_id(&id);
        drop(dom);
        resolve_node(cx, owner, id)
    }

    #[method(name = "createElement")]
    fn create_element(
        cx: &mut CallCx<'_>,
        this: This<Document>,
        tag: JsValue,
    ) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let tag = cx.string(&tag)?.to_string().to_ascii_lowercase();
        let name = QualName::new(None, ns!(html), LocalName::from(tag));
        let mut dom = dom_mut(cx)?;
        let id = dom.doc.mutate().create_element(name, Vec::new());
        drop(dom);
        resolve_node(cx, owner, Some(id))
    }

    #[method(name = "createTextNode")]
    fn create_text_node(
        cx: &mut CallCx<'_>,
        this: This<Document>,
        value: JsValue,
    ) -> JsResult<JsValue> {
        let owner = bound_document(cx, &this)?;
        let value = cx.string(&value)?.to_string();
        let mut dom = dom_mut(cx)?;
        let id = dom.doc.mutate().create_text_node(&value);
        drop(dom);
        resolve_node(cx, owner, Some(id))
    }
}

#[host_class(tag = "HTMLIFrameElement", parent = Element)]
struct HTMLIFrameElement;

fn alloc_element<T: yield_vm::HostClass>(cx: &mut CallCx<'_>, class: T) -> JsResult<Instance<T>> {
    let element = cx.alloc(class)?;
    element.init_object_field(cx, "style")?;
    Ok(element)
}

fn init_node(cx: &mut CallCx<'_>, value: &JsValue, index: u32, owner: JsValue) -> JsResult<()> {
    let node = cx.instance::<Node>(value)?;
    node.set_index(cx, JsValue::number(f64::from(index)))?;
    node.set_owner(cx, owner)
}

enum NodeClass {
    Element,
    IFrame,
    CharacterData,
}

fn resolve_node(cx: &mut CallCx<'_>, owner: JsValue, id: Option<NodeId>) -> JsResult<JsValue> {
    let Some(id) = id else {
        return Ok(JsValue::null());
    };
    let document = cx.instance::<Document>(&owner)?;
    let dom = dom_ref(cx)?;
    let lookup = if let Some(&index) = dom.handles.get(&id) {
        (index, None)
    } else {
        let index = u32::try_from(dom.nodes.len())
            .map_err(|_| JsError::type_error("too many DOM nodes"))?;
        let node = dom
            .doc
            .get_node(id)
            .ok_or_else(|| JsError::type_error("invalid DOM node handle"))?;
        let class = match &node.data {
            NodeData::Element(element) if element.name.local == local_name!("iframe") => {
                NodeClass::IFrame
            }
            NodeData::Element(_) => NodeClass::Element,
            NodeData::Text(_) | NodeData::Comment { .. } => NodeClass::CharacterData,
            _ => return Err(JsError::type_error("unsupported DOM node kind")),
        };
        (index, Some(class))
    };
    drop(dom);
    let (index, Some(class)) = lookup else {
        return document.nodes_get(cx, lookup.0 as usize);
    };
    let value = match class {
        NodeClass::Element => alloc_element(cx, Element)?.value(cx),
        NodeClass::IFrame => alloc_element(cx, HTMLIFrameElement)?.value(cx),
        NodeClass::CharacterData => cx.alloc(Node)?.value(cx),
    };
    init_node(cx, &value, index, owner)?;
    document.nodes_push(cx, value.clone())?;
    publish_node(cx, id, index);
    Ok(value)
}

// No VM operations between publishing the list entry and its native index.
fn publish_node(cx: &CallCx<'_>, id: NodeId, index: u32) {
    let mut dom = cx
        .service::<DomKey>()
        .expect("active Blitz document")
        .borrow_mut();
    debug_assert_eq!(dom.nodes.len(), index as usize);
    dom.nodes.push(id);
    dom.handles.insert(id, index);
}

#[host_module(name = "yield:dom", classes = [Node, Document, Element, HTMLIFrameElement])]
pub(super) struct DomModule {
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
        let service = cx
            .service::<DomKey>()
            .ok_or_else(|| JsError::fault("no active Blitz document"))?;
        let id = service
            .borrow()
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
        publish_node(cx, id, 0);
        Ok(value)
    }
}

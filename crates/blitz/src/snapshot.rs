//! The DOM payload stores a forest in parent-before-child order and a separate
//! wrapper-index table. Blitz node IDs never leave the live document.

use std::collections::{BTreeMap, BTreeSet};

use blitz_dom::{Attribute, BaseDocument, NodeData, NodeId, QualName};
use yield_vm::binding::CodecError;
use yield_vm::{Decode, Encode, Services};

use super::Dom;
use super::dom::DomModule;
use super::dom::{is_text_input, write_input_value};

const VERSION: u8 = 2;

/// The live tree belongs to the host; capture and restore use the current service.
#[derive(Default)]
pub(super) struct DomState;

impl Encode<DomModule> for DomState {
    fn encode(
        &self,
        out: &mut Vec<u8>,
        services: &Services<'_, DomModule>,
    ) -> Result<(), CodecError> {
        let image = DomImage::capture(&DomModule::dom(services).borrow())?;
        VERSION.encode(out, services)?;
        image.encode(out, services)
    }
}

impl Decode<DomModule> for DomState {
    fn decode(input: &mut &[u8], services: &Services<'_, DomModule>) -> Result<Self, CodecError> {
        if u8::decode(input, services)? != VERSION {
            return Err(CodecError::new("unsupported DOM snapshot version"));
        }
        let image = DomImage::decode(input, services)?;
        if !input.is_empty() {
            return Err(CodecError::new("trailing DOM snapshot bytes"));
        }
        image.restore(&mut DomModule::dom(services).borrow_mut())?;
        Ok(Self)
    }
}

#[derive(Encode, Decode)]
struct DomImage {
    nodes: Vec<NodeRecord>,
    wrappers: Vec<u32>,
}

#[derive(Encode, Decode)]
struct NodeRecord {
    kind: NodeKind,
    children: Vec<u32>,
    input_value: Option<String>,
}

enum NodeKind {
    Document,
    Element(Name, Vec<Attr>),
    Text(String),
    Comment(String),
}

#[derive(Encode, Decode)]
struct Name {
    prefix: Option<String>,
    ns: String,
    local: String,
}

#[derive(Encode, Decode)]
struct Attr {
    name: Name,
    value: String,
}

impl From<&QualName> for Name {
    fn from(name: &QualName) -> Self {
        Self {
            prefix: name.prefix.as_ref().map(ToString::to_string),
            ns: name.ns.to_string(),
            local: name.local.to_string(),
        }
    }
}

impl From<Name> for QualName {
    fn from(name: Name) -> Self {
        Self::new(
            name.prefix.as_deref().map(Into::into),
            name.ns.as_str().into(),
            name.local.as_str().into(),
        )
    }
}

impl<Owner> Encode<Owner> for NodeKind {
    fn encode(&self, out: &mut Vec<u8>, services: &Services<'_, Owner>) -> Result<(), CodecError> {
        match self {
            Self::Document => 0u8.encode(out, services),
            Self::Element(name, attrs) => {
                1u8.encode(out, services)?;
                name.encode(out, services)?;
                attrs.encode(out, services)
            }
            Self::Text(text) => {
                2u8.encode(out, services)?;
                text.encode(out, services)
            }
            Self::Comment(text) => {
                3u8.encode(out, services)?;
                text.encode(out, services)
            }
        }
    }
}

impl<Owner> Decode<Owner> for NodeKind {
    fn decode(input: &mut &[u8], services: &Services<'_, Owner>) -> Result<Self, CodecError> {
        Ok(match u8::decode(input, services)? {
            0 => Self::Document,
            1 => Self::Element(
                Name::decode(input, services)?,
                Vec::decode(input, services)?,
            ),
            2 => Self::Text(String::decode(input, services)?),
            3 => Self::Comment(String::decode(input, services)?),
            _ => return Err(CodecError::new("invalid DOM node kind")),
        })
    }
}

impl DomImage {
    fn capture(dom: &Dom) -> Result<Self, CodecError> {
        let mut nodes = Vec::new();
        let mut ids = BTreeMap::new();
        let mut stack = Vec::new();
        for start in std::iter::once(dom.doc.root_node().id).chain(dom.nodes.iter().copied()) {
            if ids.contains_key(&start) {
                continue;
            }
            // The JS wrapper may refer to an interior node of a detached tree.
            let mut root = start;
            while let Some(parent) = get_node(&dom.doc, root)?.parent {
                root = parent;
            }
            stack.push((root, None::<usize>));
            while let Some((id, parent)) = stack.pop() {
                let index = u32::try_from(nodes.len())
                    .map_err(|_| CodecError::new("too many DOM snapshot nodes"))?;
                if ids.insert(id, index).is_some() {
                    return Err(CodecError::new("duplicate live DOM node"));
                }
                let node = get_node(&dom.doc, id)?;
                let kind = match &node.data {
                    NodeData::Document(_) => NodeKind::Document,
                    NodeData::Element(element) if element.template_contents.is_none() => {
                        NodeKind::Element(
                            Name::from(&element.name),
                            element
                                .attrs
                                .iter()
                                .map(|attr| Attr {
                                    name: Name::from(&attr.name),
                                    value: attr.value.clone(),
                                })
                                .collect(),
                        )
                    }
                    NodeData::Text(text) => NodeKind::Text(text.content.clone()),
                    NodeData::Comment { contents } => NodeKind::Comment(contents.clone()),
                    _ => return Err(CodecError::new("unsupported live DOM node")),
                };
                if let Some(parent) = parent {
                    let record: &mut NodeRecord = &mut nodes[parent];
                    record.children.push(index);
                }
                nodes.push(NodeRecord {
                    kind,
                    children: Vec::with_capacity(node.children.len()),
                    input_value: node
                        .element_data()
                        .filter(|element| is_text_input(element))
                        .and_then(|element| element.text_input_data())
                        .map(|input| input.editor.text().to_string()),
                });
                stack.extend(
                    node.children
                        .iter()
                        .rev()
                        .map(|id| (*id, Some(index as usize))),
                );
            }
        }
        let wrappers = dom.nodes.iter().map(|id| ids[id]).collect();
        Ok(Self { nodes, wrappers })
    }

    fn validate(&self) -> Result<(), CodecError> {
        if !matches!(
            self.nodes.first().map(|node| &node.kind),
            Some(NodeKind::Document)
        ) {
            return Err(CodecError::new("missing DOM document node"));
        }
        let mut parented = vec![false; self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate() {
            if (index != 0 && matches!(node.kind, NodeKind::Document))
                || (!node.children.is_empty()
                    && !matches!(node.kind, NodeKind::Document | NodeKind::Element(..)))
            {
                return Err(CodecError::new("invalid DOM snapshot node"));
            }
            for &child in &node.children {
                let child = child as usize;
                if child <= index || child >= self.nodes.len() || parented[child] {
                    return Err(CodecError::new("invalid DOM snapshot child"));
                }
                parented[child] = true;
            }
        }
        let mut wrappers = BTreeSet::new();
        if self.wrappers.first().is_some_and(|&id| id != 0)
            || self
                .wrappers
                .iter()
                .any(|&id| id as usize >= self.nodes.len() || !wrappers.insert(id))
        {
            return Err(CodecError::new("invalid DOM snapshot wrapper table"));
        }
        Ok(())
    }

    fn restore(self, dom: &mut Dom) -> Result<(), CodecError> {
        self.validate()?;
        let root = dom.doc.root_node().id;
        let mut ids = Vec::with_capacity(self.nodes.len());
        let mut child_lists = Vec::with_capacity(self.nodes.len());
        let mut input_values = Vec::new();
        {
            let mut mutation = dom.doc.mutate();
            for NodeRecord {
                kind,
                children,
                input_value,
            } in self.nodes
            {
                let id = match kind {
                    NodeKind::Document => root,
                    NodeKind::Element(name, attrs) => mutation.create_element(
                        name.into(),
                        attrs
                            .into_iter()
                            .map(|attr| Attribute {
                                name: attr.name.into(),
                                value: attr.value,
                            })
                            .collect(),
                    ),
                    NodeKind::Text(text) => mutation.create_text_node(&text),
                    NodeKind::Comment(text) => mutation.create_comment_node(&text),
                };
                ids.push(id);
                child_lists.push(children);
                if let Some(value) = input_value {
                    input_values.push((id, value));
                }
            }
            // Build detached subtrees before connecting them to the document.
            let mut child_ids = Vec::new();
            for (&parent, children) in ids.iter().zip(child_lists).rev() {
                if !children.is_empty() {
                    child_ids.extend(children.into_iter().map(|index| ids[index as usize]));
                    mutation.append_children(parent, &child_ids);
                    child_ids.clear();
                }
            }
        }
        for (id, value) in input_values {
            if !write_input_value(&mut dom.doc, id, &value) {
                return Err(CodecError::new("invalid DOM snapshot input value"));
            }
        }
        dom.nodes = self
            .wrappers
            .into_iter()
            .map(|index| ids[index as usize])
            .collect();
        dom.handles = dom
            .nodes
            .iter()
            .enumerate()
            .map(|(index, &id)| (id, index as u32))
            .collect();
        Ok(())
    }
}

fn get_node(doc: &BaseDocument, id: NodeId) -> Result<&blitz_dom::Node, CodecError> {
    doc.get_node(id)
        .ok_or_else(|| CodecError::new("missing live DOM node"))
}

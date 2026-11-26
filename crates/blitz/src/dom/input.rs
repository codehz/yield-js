//! Bind input accessors to the Blitz text editor and checkbox state.

use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::node::{SpecialElementData, TextInputData};
use blitz_dom::{BaseDocument, ElementData, NodeId, QualName, local_name, ns};
use yield_binding_macros::{host_class, methods};
use yield_vm::intrinsics::TypeError;
use yield_vm::{ClassCx, JsError, JsResult, JsValue, This};

use super::{Dom, element::HTMLElement};

#[host_class(tag = "HTMLInputElement", parent = HTMLElement, methods, services = [dom: Rc<RefCell<Dom>>])]
pub(super) struct HTMLInputElement;

#[methods(HTMLInputElement)]
impl HTMLInputElement {
    #[get(name = "checked")]
    fn checked(cx: &mut ClassCx<'_, '_, Self>, this: This<Self>) -> JsResult<JsValue> {
        let index = receiver_index!(cx, this)?;
        let dom = Self::dom(cx.services()).borrow();
        let element = dom.node(index)?.element_data().unwrap();
        Ok(JsValue::boolean(checkbox_checked(element)))
    }

    #[set(name = "checked")]
    fn set_checked(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Self>,
        value: JsValue,
    ) -> JsResult<()> {
        let index = receiver_index!(cx, this)?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let node = dom.node(index)?;
        if is_checkbox(node.element_data().unwrap()) {
            let id = node.id;
            dom.dirty_checkboxes.insert(id);
            write_checkbox_checked(&mut dom.doc, id, value.truthy());
        }
        Ok(())
    }

    #[get(name = "defaultChecked")]
    fn default_checked(cx: &mut ClassCx<'_, '_, Self>, this: This<Self>) -> JsResult<JsValue> {
        let index = receiver_index!(cx, this)?;
        let dom = Self::dom(cx.services()).borrow();
        Ok(JsValue::boolean(
            dom.node(index)?.attr(local_name!("checked")).is_some(),
        ))
    }

    #[set(name = "defaultChecked")]
    fn set_default_checked(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Self>,
        value: JsValue,
    ) -> JsResult<()> {
        let index = receiver_index!(cx, this)?;
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.node(index)?.id;
        dom.mutate_checked_attribute(id, value.truthy().then_some(""));
        Ok(())
    }

    #[get(name = "value")]
    fn value(cx: &mut ClassCx<'_, '_, Self>, this: This<Self>) -> JsResult<JsValue> {
        let index = receiver_index!(cx, this)?;
        let dom = Self::dom(cx.services()).borrow();
        let element = dom.node(index)?.element_data().unwrap();
        Ok(match element.text_input_data() {
            Some(input) => {
                let mut parts = input.editor.text().into_iter();
                let first = parts.next().unwrap();
                let second = parts.next().unwrap();
                if second.is_empty() {
                    JsValue::string(first)
                } else {
                    JsValue::string([first, second].concat())
                }
            }
            None => JsValue::string(element.attr(local_name!("value")).unwrap_or("")),
        })
    }

    #[set(name = "value")]
    fn set_value(cx: &mut ClassCx<'_, '_, Self>, this: This<Self>, value: JsValue) -> JsResult<()> {
        let index = receiver_index!(cx, this)?;
        let value = cx.string(&value)?.to_string();
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.node(index)?.id;
        if !write_input_value(&mut dom.doc, id, &value) {
            return Err(JsError::new::<TypeError>(
                "input does not support a text value",
            ));
        }
        Ok(())
    }

    #[set(name = "defaultValue")]
    fn set_default_value(
        cx: &mut ClassCx<'_, '_, Self>,
        this: This<Self>,
        value: JsValue,
    ) -> JsResult<()> {
        let index = receiver_index!(cx, this)?;
        let value = cx.string(&value)?.to_string();
        let mut dom = Self::dom(cx.services()).borrow_mut();
        let id = dom.node(index)?.id;
        let element = dom
            .doc
            .get_node_mut(id)
            .unwrap()
            .element_data_mut()
            .unwrap();
        // Blitz's value-attribute mutation also overwrites the editor. Preserve
        // an existing live value and selection when updating the default.
        let editor = matches!(element.special_data, SpecialElementData::TextInput(_))
            .then(|| std::mem::take(&mut element.special_data));
        dom.doc.mutate().set_attribute(
            id,
            QualName::new(None, ns!(), local_name!("value")),
            &value,
        );
        if let Some(editor) = editor {
            dom.doc
                .get_node_mut(id)
                .unwrap()
                .element_data_mut()
                .unwrap()
                .special_data = editor;
        }
        Ok(())
    }

    #[get(name = "defaultValue")]
    fn default_value(cx: &mut ClassCx<'_, '_, Self>, this: This<Self>) -> JsResult<JsValue> {
        let index = receiver_index!(cx, this)?;
        let dom = Self::dom(cx.services()).borrow();
        Ok(JsValue::string(
            dom.node(index)?.attr(local_name!("value")).unwrap_or(""),
        ))
    }
}

pub(crate) fn is_input(element: &ElementData) -> bool {
    element.name.ns == ns!(html) && element.name.local == local_name!("input")
}

pub(crate) fn is_text_input(element: &ElementData) -> bool {
    is_input(element)
        && matches!(
            element.attr(local_name!("type")),
            None | Some("text" | "password" | "email" | "number" | "search" | "tel" | "url")
        )
}

pub(crate) fn is_checkbox(element: &ElementData) -> bool {
    is_input(element) && element.attr(local_name!("type")) == Some("checkbox")
}

pub(crate) fn checkbox_checked(element: &ElementData) -> bool {
    element
        .checkbox_input_checked()
        .unwrap_or_else(|| element.has_attr(local_name!("checked")))
}

pub(crate) fn write_checkbox_checked(doc: &mut BaseDocument, id: NodeId, checked: bool) {
    if doc
        .get_node(id)
        .unwrap()
        .element_data()
        .unwrap()
        .checkbox_input_checked()
        == Some(checked)
    {
        return;
    }
    // Snapshot the old selector state before touching the value used by paint.
    doc.snapshot_node_state_only(id);
    let node = doc.get_node_mut(id).unwrap();
    let element = node.element_data_mut().unwrap();
    element.special_data = SpecialElementData::CheckboxInput(checked);
    element.set_checkbox_input_checked(checked);
    node.mark_ancestors_dirty();
}

impl Dom {
    pub(super) fn mutate_checked_attribute(&mut self, id: NodeId, value: Option<&str>) {
        let element = self.doc.get_node(id).unwrap().element_data().unwrap();
        let current = is_checkbox(element).then(|| checkbox_checked(element));
        // Initialize before mutation so Blitz doesn't interpret a textual
        // checked attribute as a separate live-value assignment.
        if let Some(checked) = current {
            write_checkbox_checked(&mut self.doc, id, checked);
        }
        let name = QualName::new(None, ns!(), local_name!("checked"));
        if let Some(value) = value {
            self.doc.mutate().set_attribute(id, name, value);
        } else {
            self.doc.mutate().clear_attribute(id, name);
        }
        if let Some(current) = current {
            let checked = if self.dirty_checkboxes.contains(&id) {
                current
            } else {
                value.is_some()
            };
            write_checkbox_checked(&mut self.doc, id, checked);
        }
    }
}

/// Write the editor rather than the value attribute. Before first layout there
/// may not be an editor yet; layout will style the one we initialize here.
pub(crate) fn write_input_value(doc: &mut BaseDocument, id: NodeId, value: &str) -> bool {
    let Some(element) = doc
        .get_node_mut(id)
        .and_then(|node| node.element_data_mut())
        .filter(|element| is_text_input(element))
    else {
        return false;
    };
    match &mut element.special_data {
        SpecialElementData::TextInput(input) => {
            if input.editor.text() != value {
                input.editor.set_text(value);
                doc.with_text_input(id, |mut driver| {
                    driver.refresh_layout();
                });
            }
        }
        SpecialElementData::None => {
            let mut input = TextInputData::new(false);
            input.editor.set_text(value);
            element.special_data = SpecialElementData::TextInput(input);
        }
        _ => return false,
    }
    true
}

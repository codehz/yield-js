//! `yield:dom` host module: DOM node wrappers bound to the Blitz document.
//!
//! `resolver.rs` holds the shared `Dom` helpers that map between node ids
//! and JS handle instances.

// Each class owns one or more tags. `from module` names an existing binding.
// A class can specify a non-default parent with the same syntax as a type bound.
macro_rules! html_elements {
    ($consumer:ident $($extra:tt)*) => {
        $consumer! {
            {
                (["iframe"], HTMLIFrameElement, construct_iframe),
                (["input"], HTMLInputElement, construct_input, from input),
                (["button"], HTMLButtonElement, construct_button),
                (["label"], HTMLLabelElement, construct_label),
                (["ul"], HTMLUListElement, construct_ul),
                (["li"], HTMLLIElement, construct_li),
                (["div"], HTMLDivElement, construct_div),
                (["span"], HTMLSpanElement, construct_span),
                (["h1", "h2", "h3", "h4", "h5", "h6"], HTMLHeadingElement, construct_heading),
                (["p"], HTMLParagraphElement, construct_paragraph),
                (["ol"], HTMLOListElement, construct_ol),
                (["blockquote", "q"], HTMLQuoteElement, construct_quote),
                (["pre"], HTMLPreElement, construct_pre),
                (["br"], HTMLBRElement, construct_br),
                (["hr"], HTMLHRElement, construct_hr),
                (["a"], HTMLAnchorElement, construct_a),
                (["area"], HTMLAreaElement, construct_area),
                (["audio"], HTMLAudioElement : HTMLMediaElement, construct_audio),
                (["base"], HTMLBaseElement, construct_base),
                (["body"], HTMLBodyElement, construct_body),
                (["canvas"], HTMLCanvasElement, construct_canvas),
                (["caption"], HTMLTableCaptionElement, construct_caption),
                (["col", "colgroup"], HTMLTableColElement, construct_col),
                (["data"], HTMLDataElement, construct_data),
                (["datalist"], HTMLDataListElement, construct_datalist),
                (["del", "ins"], HTMLModElement, construct_del),
                (["details"], HTMLDetailsElement, construct_details),
                (["dialog"], HTMLDialogElement, construct_dialog),
                (["dl"], HTMLDListElement, construct_dl),
                (["embed"], HTMLEmbedElement, construct_embed),
                (["fieldset"], HTMLFieldSetElement, construct_fieldset),
                (["form"], HTMLFormElement, construct_form),
                (["head"], HTMLHeadElement, construct_head),
                (["html"], HTMLHtmlElement, construct_html),
                (["img"], HTMLImageElement, construct_img),
                (["legend"], HTMLLegendElement, construct_legend),
                (["link"], HTMLLinkElement, construct_link),
                (["map"], HTMLMapElement, construct_map),
                (["menu"], HTMLMenuElement, construct_menu),
                (["meta"], HTMLMetaElement, construct_meta),
                (["meter"], HTMLMeterElement, construct_meter),
                (["object"], HTMLObjectElement, construct_object),
                (["optgroup"], HTMLOptGroupElement, construct_optgroup),
                (["option"], HTMLOptionElement, construct_option),
                (["output"], HTMLOutputElement, construct_output),
                (["picture"], HTMLPictureElement, construct_picture),
                (["progress"], HTMLProgressElement, construct_progress),
                (["script"], HTMLScriptElement, construct_script),
                (["select"], HTMLSelectElement, construct_select),
                (["selectedcontent"], HTMLSelectedContentElement, construct_selectedcontent),
                (["slot"], HTMLSlotElement, construct_slot),
                (["source"], HTMLSourceElement, construct_source),
                (["style"], HTMLStyleElement, construct_style),
                (["table"], HTMLTableElement, construct_table),
                (["tbody", "tfoot", "thead"], HTMLTableSectionElement, construct_tbody),
                (["td", "th"], HTMLTableCellElement, construct_td),
                (["template"], HTMLTemplateElement, construct_template),
                (["textarea"], HTMLTextAreaElement, construct_textarea),
                (["time"], HTMLTimeElement, construct_time),
                (["title"], HTMLTitleElement, construct_title),
                (["tr"], HTMLTableRowElement, construct_tr),
                (["track"], HTMLTrackElement, construct_track),
                (["video"], HTMLVideoElement : HTMLMediaElement, construct_video),
            }
            $($extra)*
        }
    };
}

#[yield_binding_macros::host_class(tag, parent = element::HTMLElement)]
struct HTMLMediaElement;

macro_rules! declare_html_element {
    ($class:ident) => {
        #[yield_binding_macros::host_class(tag, parent = element::HTMLElement)]
        struct $class;
    };
    ($class:ident : $parent:ident) => {
        #[yield_binding_macros::host_class(tag, parent = $parent)]
        struct $class;
    };
    ($class:ident, from $owner:ident) => {};
}

macro_rules! declare_html_elements {
    ({ $(($tags:tt, $class:ident $(: $parent:ident)?, $ctor:ident $(, from $owner:ident)?),)* }) => {
        $(declare_html_element!($class $(: $parent)? $(, from $owner)?);)*
    };
}

html_elements!(declare_html_elements);

macro_rules! receiver_index {
    ($cx:ident, $this:ident) => {{
        let wrapper = $this.instance().value($cx);
        super::base_node_index($cx, &wrapper)
    }};
}

mod document;
mod element;
mod input;
mod module;
mod node;
mod resolver;

use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::NodeData;
use yield_vm::binding::CallCx;
use yield_vm::intrinsics::TypeError;
use yield_vm::{Instance, JsError, JsResult, JsValue};

use self::node::NodeSlots;
use super::Dom;

pub(crate) use input::{
    checkbox_checked, is_checkbox, is_text_input, write_checkbox_checked, write_input_value,
};
pub use module::DomModule;
pub(crate) use node::Node;

// Declaration-local slot number of `Node::index`.
pub(crate) const NODE_INDEX_SLOT: usize = 0;

/// Resolve the numeric node index stored on a `Node` instance slot.
pub(super) fn node_index(cx: &mut CallCx<'_>, node: Instance<Node>) -> JsResult<u32> {
    let Some(index) = node.index(cx)?.as_number() else {
        let error =
            cx.construct::<TypeError>(&[JsValue::string("DOM node is not bound to a document")])?;
        return Err(cx.throw(error));
    };
    Ok(index as u32)
}

/// Resolve the `Node` instance behind an `Element`/`Document` subclass value
/// and return its node index.
pub(super) fn base_node_index(cx: &mut CallCx<'_>, value: &JsValue) -> JsResult<u32> {
    let node = cx.instance::<Node>(value)?;
    node_index(cx, node)
}

fn node_name(dom: &Rc<RefCell<Dom>>, index: u32) -> JsResult<JsValue> {
    let dom = dom.borrow();
    let node = dom.node(index)?;
    Ok(match &node.data {
        NodeData::Element(element) => JsValue::string(element.name.local.to_uppercase()),
        NodeData::Text(_) => JsValue::string("#text"),
        NodeData::Comment { .. } => JsValue::string("#comment"),
        NodeData::Document(_) => JsValue::string("#document"),
        _ => return Err(JsError::new::<TypeError>("unsupported DOM node kind")),
    })
}

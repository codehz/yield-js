//! Adapt Blitz's native event driver to the live session without holding a
//! document borrow during guest callbacks.

use std::cell::{Ref, RefMut};

use blitz_dom::{DocGuard, DocGuardMut, Document, EventDriver, EventHandler, NodeId};
use blitz_traits::events::{DomEvent, DomEventData, EventState, MouseEventButton, UiEvent};

use crate::{BlitzSession, Dom, UserEvent};

impl Document for BlitzSession {
    fn inner(&self) -> DocGuard<'_> {
        DocGuard::RefCell(self.document())
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        DocGuardMut::RefCell(RefMut::map(self.dom.borrow_mut(), |dom| &mut dom.doc))
    }

    fn poll(&mut self, cx: Option<std::task::Context<'_>>) -> bool {
        if let Some(cx) = cx {
            self.timers.set_waker(cx.waker());
        }
        match self.drain_turns() {
            Ok(woke) => woke,
            Err(error) => {
                eprintln!("yield-blitz: {error}");
                false
            }
        }
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        // The driver and the session access the same document in separate,
        // short borrows, so listeners can mutate it before default actions.
        let mut document = WindowDocument(self.dom.clone());
        EventDriver::new(&mut document, GuestEventHandler(self)).handle_ui_event(event);
        if let Err(error) = self.run_until_idle() {
            eprintln!("yield-blitz: {error}");
        }
    }
}

struct WindowDocument(std::rc::Rc<std::cell::RefCell<Dom>>);

impl Document for WindowDocument {
    fn inner(&self) -> DocGuard<'_> {
        DocGuard::RefCell(Ref::map(self.0.borrow(), |dom| &dom.doc))
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        DocGuardMut::RefCell(RefMut::map(self.0.borrow_mut(), |dom| &mut dom.doc))
    }
}

struct GuestEventHandler<'a>(&'a mut BlitzSession);

impl EventHandler for GuestEventHandler<'_> {
    fn handle_event(
        &mut self,
        _: &[NodeId],
        event: &mut DomEvent,
        document: &mut dyn Document,
        state: &mut EventState,
    ) {
        if !event.source.is_trusted() {
            return;
        }
        let (kind, target) = match &event.data {
            DomEventData::Click(pointer) if pointer.button == MouseEventButton::Main => {
                let doc = document.inner();
                let mut node = doc.get_node(event.target);
                let target = loop {
                    match node {
                        Some(hit) if hit.element_data().is_some() => break Some(hit.id),
                        Some(hit) => node = hit.parent.and_then(|id| doc.get_node(id)),
                        None => break None,
                    }
                };
                (UserEvent::Click, target)
            }
            DomEventData::Input(_) => {
                let doc = document.inner();
                let target = doc
                    .get_node(event.target)
                    .and_then(|node| node.element_data())
                    .filter(|element| {
                        crate::dom::is_text_input(element) && element.text_input_data().is_some()
                    })
                    .map(|_| event.target);
                (UserEvent::Input, target)
            }
            _ => return,
        };
        if let Some(target) = target {
            match self.0.dispatch_user_event(kind, target) {
                Ok((false, _)) => state.prevent_default(),
                Ok((true, true)) => {
                    // The VM already toggled before dispatch. Preserve focus
                    // without repeating Blitz's checkbox default action.
                    state.prevent_default();
                    focus_checkbox(document, target);
                }
                Ok((true, false)) if matches!(kind, UserEvent::Click) => {
                    let checkbox = {
                        let doc = document.inner();
                        let label = doc.get_node(target).and_then(|node| node.element_data());
                        if !label.is_some_and(|el| {
                            el.name.ns == blitz_dom::ns!(html)
                                && el.name.local == blitz_dom::local_name!("label")
                        }) {
                            return;
                        }
                        doc.label_bound_input_element(target)
                            .filter(|node| node.element_data().is_some_and(crate::dom::is_checkbox))
                            .map(|node| node.id)
                    };
                    if let Some(checkbox) = checkbox {
                        state.prevent_default();
                        match self.0.dispatch_user_event(kind, checkbox) {
                            Ok((true, true)) => focus_checkbox(document, checkbox),
                            Ok(_) => {}
                            Err(error) => eprintln!("yield-blitz: {error}"),
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => eprintln!("yield-blitz: {error}"),
            }
        }
    }
}

fn focus_checkbox(document: &mut dyn Document, target: NodeId) {
    let mut doc = document.inner_mut();
    if doc
        .get_node(target)
        .is_some_and(|node| node.flags.is_in_document())
    {
        doc.set_focus_to(target);
    }
}

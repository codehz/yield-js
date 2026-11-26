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
                Ok(false) => state.prevent_default(),
                Ok(true) => {}
                Err(error) => eprintln!("yield-blitz: {error}"),
            }
        }
    }
}

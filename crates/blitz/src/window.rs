//! Adapt Blitz's native event driver to the live session without holding a
//! document borrow during guest callbacks.

use std::cell::{Ref, RefMut};

use blitz_dom::{DocGuard, DocGuardMut, Document, EventDriver, EventHandler, NodeId};
use blitz_traits::events::{DomEvent, DomEventData, EventState, MouseEventButton, UiEvent};

use crate::{BlitzSession, Dom};

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
        EventDriver::new(&mut document, ClickHandler(self)).handle_ui_event(event);
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

struct ClickHandler<'a>(&'a mut BlitzSession);

impl EventHandler for ClickHandler<'_> {
    fn handle_event(
        &mut self,
        _: &[NodeId],
        event: &mut DomEvent,
        document: &mut dyn Document,
        state: &mut EventState,
    ) {
        if !event.source.is_trusted()
            || !matches!(&event.data, DomEventData::Click(pointer) if pointer.button == MouseEventButton::Main)
        {
            return;
        }
        let target = {
            let doc = document.inner();
            let mut node = doc.get_node(event.target);
            loop {
                match node {
                    Some(hit) if hit.element_data().is_some() => break Some(hit.id),
                    Some(hit) => node = hit.parent.and_then(|id| doc.get_node(id)),
                    None => break None,
                }
            }
        };
        if let Some(target) = target {
            match self.0.dispatch_click(target) {
                Ok(false) => state.prevent_default(),
                Ok(true) => {}
                Err(error) => eprintln!("yield-blitz: {error}"),
            }
        }
    }
}

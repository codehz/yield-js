//! Statically built service views and the host-only type lookup table.

use std::any::{Any, TypeId};
use std::marker::PhantomData;

#[doc(hidden)]
pub trait Provide<T, I> {
    fn provide(&self) -> &T;
}

#[doc(hidden)]
pub struct Slot<const N: usize>;

#[doc(hidden)]
pub fn assert_unique<T, L: Provide<T, I>, I>(_: &L) {}

/// The generated field view for a host binding type.
///
/// # Safety
/// Accessors must read only this owner's view and treat its projections as shared.
#[doc(hidden)]
pub unsafe trait ServiceOwner: 'static {
    type View: 'static;
}

/// Build the owner's projections from the provided layout.
///
/// # Safety
/// The view must be valid for shared access as long as the layout stays live at
/// the same address. Dropping it must not access the layout.
#[doc(hidden)]
pub unsafe trait BuildView<L, I>: ServiceOwner {
    fn build_view(layout: &L) -> Self::View;
}

#[doc(hidden)]
pub struct ServiceField {
    type_id: TypeId,
    offset: usize,
}

impl ServiceField {
    pub const fn of<T: Any>(offset: usize) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            offset,
        }
    }
}

/// Owns the layout without moving it after views have been constructed.
#[doc(hidden)]
pub struct ServiceStorage {
    layout: *mut dyn Any,
    fields: Box<[ServiceField]>,
}

impl Default for ServiceStorage {
    fn default() -> Self {
        Self {
            layout: Box::into_raw(Box::new(())),
            fields: Box::default(),
        }
    }
}

impl Drop for ServiceStorage {
    fn drop(&mut self) {
        // SAFETY: storage exclusively owns this original Box allocation.
        drop(unsafe { Box::from_raw(self.layout) });
    }
}

impl ServiceStorage {
    /// Build all views from the final boxed layout before handing either one to
    /// the declaration assembler.
    ///
    /// # Safety
    /// Each descriptor must identify a real, aligned field of its exact type.
    /// The layout must not be packed and remains shared through all views.
    pub unsafe fn with_layout<L: Any, R>(
        layout: Box<L>,
        fields: impl Into<Box<[ServiceField]>>,
        build: impl FnOnce(&L) -> R,
    ) -> (Self, R) {
        let mut fields = fields.into();
        fields.sort_unstable_by_key(|field| field.type_id);
        let layout = Box::into_raw(layout);
        let storage = Self { layout, fields };
        // SAFETY: storage owns the stable allocation, including during unwind.
        let views = build(unsafe { &*layout });
        (storage, views)
    }

    fn offset(&self, type_id: TypeId) -> Option<usize> {
        let index = self
            .fields
            .binary_search_by_key(&type_id, |field| field.type_id)
            .ok()?;
        Some(self.fields[index].offset)
    }

    pub(crate) fn bind<T: Any>(&self) -> Option<&T> {
        let offset = self.offset(TypeId::of::<T>())?;
        // SAFETY: descriptor was selected by exact type and belongs to this layout.
        Some(unsafe { project(self.layout.cast::<u8>(), offset) })
    }
}

/// Borrowed declaration-specific service view.
pub struct Services<'a, Owner = ()> {
    view: &'a dyn Any,
    owner: PhantomData<fn(Owner) -> Owner>,
}

impl<Owner> Copy for Services<'_, Owner> {}
impl<Owner> Clone for Services<'_, Owner> {
    fn clone(&self) -> Self {
        *self
    }
}

impl Default for Services<'_> {
    fn default() -> Self {
        Self {
            view: &(),
            owner: PhantomData,
        }
    }
}

impl<'a> Services<'a> {
    /// # Safety
    /// The view and its projections remain valid for `'a`; its layout remains
    /// owned by the same binding table for that whole lifetime.
    pub(crate) unsafe fn from_view(view: &'a dyn Any) -> Self {
        Self {
            view,
            owner: PhantomData,
        }
    }

    /// # Safety
    /// The view belongs to `Owner`, and its layout stays live for `'a`.
    pub(crate) unsafe fn for_owner<Owner: ServiceOwner>(self) -> Services<'a, Owner> {
        Services {
            view: self.view,
            owner: PhantomData,
        }
    }
}

impl<'a, Owner: ServiceOwner> Services<'a, Owner> {
    /// # Safety
    /// The caller pairs this borrow with Owner's view and its stable live layout.
    #[doc(hidden)]
    #[inline]
    pub unsafe fn __view(&self) -> &'a Owner::View {
        // SAFETY: guaranteed by the caller constructing the erased view borrow.
        unsafe { &*(self.view as *const dyn Any).cast::<Owner::View>() }
    }
}

/// # Safety
/// `base` must point to a live layout with an aligned `T` at `offset`.
unsafe fn project<'a, T>(base: *const u8, offset: usize) -> &'a T {
    unsafe { &*base.add(offset).cast::<T>() }
}

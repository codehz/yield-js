//! Link-time service resolution for erased, host-owned field layouts.

use std::any::{Any, TypeId, type_name};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use crate::fetch::FetchService;
use crate::host::{
    ConsoleSink, CryptoService, Diagnostics, GcRoots, SubtleCryptoService, TimeService,
    TimerService, UserAgent,
};

const MISSING: usize = usize::MAX;

/// The single ordered dependency declaration for a host binding type.
///
/// # Safety
/// Each generated accessor must use the slot matching its exact return type.
#[doc(hidden)]
pub unsafe trait ServiceOwner: 'static {
    const SERVICE_DEPENDENCIES: &'static [ServiceDependency];
}

#[doc(hidden)]
pub struct ServiceDependency {
    type_id: TypeId,
    type_name: fn() -> &'static str,
}

impl ServiceDependency {
    // Infer the type from the accessor instead of expanding its type twice.
    pub const fn of_accessor<Owner, T: Any>(
        _: fn(&Services<'static, Owner>) -> &'static T,
    ) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>,
        }
    }
}

#[doc(hidden)]
pub struct ServiceField {
    type_id: TypeId,
    type_name: fn() -> &'static str,
    offset: usize,
}

impl ServiceField {
    pub const fn of<T: Any>(offset: usize) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>,
            offset,
        }
    }
}

/// Owns the entire generated Rust struct. `Any` owns/drops it, but is never
/// used to inspect its dynamic type.
#[doc(hidden)]
pub struct ServiceStorage {
    layout: Box<dyn Any>,
    fields: Box<[ServiceField]>,
}

impl Default for ServiceStorage {
    fn default() -> Self {
        Self {
            layout: Box::new(()),
            fields: Box::default(),
        }
    }
}

impl ServiceStorage {
    /// # Safety
    /// Every descriptor must identify a real, aligned field of precisely its
    /// declared type in `layout`. The field must remain valid for shared
    /// access through the boxed layout (in particular, no packed fields).
    pub unsafe fn from_layout(
        layout: impl Any,
        fields: impl Into<Box<[ServiceField]>>,
    ) -> Result<Self, String> {
        let mut fields = fields.into();
        fields.sort_unstable_by_key(|field| field.type_id);
        for pair in fields.windows(2) {
            if pair[0].type_id == pair[1].type_id {
                return Err(format!(
                    "duplicate host service {:?}",
                    (pair[0].type_name)()
                ));
            }
        }
        Ok(Self {
            layout: Box::new(layout),
            fields,
        })
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
        // SAFETY: the descriptor was selected by its exact type and belongs
        // to this boxed layout.
        Some(unsafe { project(base(self.layout.as_ref()), offset) })
    }

    pub(crate) fn resolve(
        &self,
        dependencies: &[ServiceDependency],
    ) -> Result<ResolvedServices, &'static str> {
        let mut offsets = Vec::with_capacity(dependencies.len());
        for dependency in dependencies {
            offsets.push(
                self.offset(dependency.type_id)
                    .ok_or_else(dependency.type_name)?,
            );
        }
        Ok(ResolvedServices(offsets))
    }

    /// # Safety
    /// `resolved` must have been resolved against this storage (or be empty).
    pub(crate) unsafe fn view<'a>(&'a self, resolved: &'a ResolvedServices) -> Services<'a> {
        Services {
            layout: self.layout.as_ref(),
            offsets: &resolved.0,
            owner: PhantomData,
        }
    }
}

pub(crate) struct ResolvedServices(Vec<usize>);

impl ResolvedServices {
    pub(crate) const EMPTY: Self = Self(Vec::new());
}

// Built-in consumers have a closed, fixed set of optional offsets. Keep it
// inline so constructing an empty Host does not allocate a scope table.
macro_rules! builtin_services {
    ($($name:ident: $ty:ty),* $(,)?) => {
        pub(crate) struct BuiltinOffsets { $($name: usize),* }

        impl BuiltinOffsets {
            pub(crate) fn resolve(storage: &ServiceStorage) -> Self {
                Self { $($name: storage.offset(TypeId::of::<$ty>()).unwrap_or(MISSING)),* }
            }

            /// # Safety
            /// These offsets must have been resolved against `storage`.
            pub(crate) unsafe fn bind<'a>(&'a self, storage: &'a ServiceStorage) -> BuiltinServices<'a> {
                BuiltinServices { base: base(storage.layout.as_ref()), offsets: self }
            }
        }

        pub(crate) struct BuiltinServices<'a> {
            base: *const u8,
            offsets: &'a BuiltinOffsets,
        }

        impl<'a> BuiltinServices<'a> {
            $(pub(crate) fn $name(&self) -> Option<&'a $ty> {
                let offset = self.offsets.$name;
                if offset == MISSING {
                    None
                } else {
                    // SAFETY: this slot was resolved for this type and layout.
                    Some(unsafe { project(self.base, offset) })
                }
            })*
        }
    };
}

builtin_services! {
    console: Arc<dyn ConsoleSink>,
    crypto: Arc<dyn CryptoService>,
    diagnostics: Rc<dyn Diagnostics>,
    fetch: Arc<dyn FetchService>,
    gc_roots: Rc<dyn GcRoots>,
    subtle_crypto: Arc<dyn SubtleCryptoService>,
    time: Arc<dyn TimeService>,
    timer: Arc<dyn TimerService>,
    user_agent: UserAgent,
}

/// The linked services for `Owner`. Accessors are generated next to its declaration.
pub struct Services<'a, Owner = ()> {
    layout: &'a dyn Any,
    offsets: &'a [usize],
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
            layout: &(),
            offsets: &[],
            owner: PhantomData,
        }
    }
}

impl<'a> Services<'a> {
    /// # Safety
    /// The view must have been linked from `Owner::SERVICE_DEPENDENCIES`.
    pub(crate) unsafe fn for_owner<Owner: ServiceOwner>(self) -> Services<'a, Owner> {
        Services {
            layout: self.layout,
            offsets: self.offsets,
            owner: PhantomData,
        }
    }
}

impl<'a, Owner: ServiceOwner> Services<'a, Owner> {
    /// # Safety
    /// `index` must be the declared slot for `T` in `Owner`.
    #[doc(hidden)]
    #[inline]
    pub unsafe fn __get<T: Any>(&self, index: usize) -> &'a T {
        // SAFETY: linking populated this required slot for T in this layout.
        unsafe { project(base(self.layout), *self.offsets.get_unchecked(index)) }
    }
}

fn base(layout: &dyn Any) -> *const u8 {
    (layout as *const dyn Any).cast::<u8>()
}

/// # Safety
/// The base must have provenance over a live layout with a valid, aligned `T`
/// at `offset` which can be shared for `'a`.
unsafe fn project<'a, T>(base: *const u8, offset: usize) -> &'a T {
    unsafe { &*base.add(offset).cast::<T>() }
}

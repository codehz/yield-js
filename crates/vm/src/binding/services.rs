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

/// The service dependencies of a module, class, or operation.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct ServiceScope {
    owner: Option<TypeId>,
    dependencies: &'static [ServiceDependency],
}

impl ServiceScope {
    pub(crate) const EMPTY: Self = Self {
        owner: None,
        dependencies: &[],
    };

    pub const fn of<S: ServiceSet>() -> Self {
        Self {
            owner: Some(TypeId::of::<S>()),
            dependencies: S::DEPENDENCIES,
        }
    }
}

/// # Safety
/// The dependency at each generated accessor's index must have the exact
/// accessor type and requiredness. A set's type identifies that one ordered
/// dependency list.
#[doc(hidden)]
pub unsafe trait ServiceSet: 'static {
    const DEPENDENCIES: &'static [ServiceDependency];
}

#[doc(hidden)]
pub struct ServiceDependency {
    type_id: TypeId,
    type_name: fn() -> &'static str,
    required: bool,
}

impl ServiceDependency {
    pub const fn required<T: Any>() -> Self {
        Self {
            required: true,
            ..Self::optional::<T>()
        }
    }

    pub const fn optional<T: Any>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>,
            required: false,
        }
    }

    // Infer the type from the accessor so its declaration and this metadata
    // cannot disagree, even if its return type comes from another macro.
    pub const fn for_required<S, T: Any>(_: fn(&S) -> &'static T) -> Self {
        Self::required::<T>()
    }

    pub const fn for_optional<S, T: Any>(_: fn(&S) -> Option<&'static T>) -> Self {
        Self::optional::<T>()
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

    pub(crate) fn resolve(&self, scope: ServiceScope) -> Result<ResolvedServices, &'static str> {
        let offsets = scope
            .dependencies
            .iter()
            .map(|dependency| match self.offset(dependency.type_id) {
                Some(offset) => Ok(offset),
                None if dependency.required => Err((dependency.type_name)()),
                None => Ok(MISSING),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedServices {
            owner: scope.owner,
            offsets,
        })
    }

    /// # Safety
    /// `resolved` must have been resolved against this storage (or be empty).
    pub(crate) unsafe fn view<'a>(&'a self, resolved: &'a ResolvedServices) -> Services<'a> {
        Services {
            layout: self.layout.as_ref(),
            resolved,
        }
    }
}

pub(crate) struct ResolvedServices {
    owner: Option<TypeId>,
    offsets: Vec<usize>,
}

impl ResolvedServices {
    pub(crate) const EMPTY: Self = Self {
        owner: None,
        offsets: Vec::new(),
    };
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

/// The already-resolved service dependencies for the current callback.
/// Obtain typed accessors with the scope declared on that callback's owner.
#[derive(Clone, Copy)]
pub struct Services<'a> {
    layout: &'a dyn Any,
    resolved: &'a ResolvedServices,
}

impl Default for Services<'_> {
    fn default() -> Self {
        const EMPTY: &ResolvedServices = &ResolvedServices::EMPTY;
        Self {
            layout: &(),
            resolved: EMPTY,
        }
    }
}

impl<'a> Services<'a> {
    #[doc(hidden)]
    #[inline]
    pub fn bind<S: ServiceSet>(&self) -> BoundServices<'a, S> {
        assert_eq!(
            self.resolved.owner,
            Some(TypeId::of::<S>()),
            "wrong host service scope"
        );
        // SAFETY: the resolved descriptor has just been checked to belong to S.
        BoundServices {
            base: base(self.layout),
            offsets: &self.resolved.offsets,
            scope: PhantomData,
        }
    }
}

/// Typed proof of a scope bound to one particular host allocation.
#[doc(hidden)]
pub struct BoundServices<'a, S> {
    base: *const u8,
    offsets: &'a [usize],
    scope: PhantomData<fn(S) -> S>,
}

impl<'a, S: ServiceSet> BoundServices<'a, S> {
    /// # Safety
    /// `index` must be a dependency on `T` in `S`.
    pub unsafe fn get<T: Any>(&self, index: usize) -> Option<&'a T> {
        let offset = unsafe { *self.offsets.get_unchecked(index) };
        if offset == MISSING {
            None
        } else {
            // SAFETY: the slot is for T and was resolved against this layout.
            Some(unsafe { project(self.base, offset) })
        }
    }

    /// # Safety
    /// `index` must be a required dependency on `T` in `S`.
    pub unsafe fn get_required<T: Any>(&self, index: usize) -> &'a T {
        let offset = unsafe { *self.offsets.get_unchecked(index) };
        // SAFETY: linking rejects a missing required slot; this slot is for T.
        unsafe { project(self.base, offset) }
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

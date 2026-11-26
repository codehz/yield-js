use super::FUNCTION_PARENT;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};

const NAME: &str = "queueMicrotask";
const KEY: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const OWNER: Owner<P> = Owner::new(
        NAME,
        &[RootSpec::new(NAME, RootKind::Native(KEY), FUNCTION_PARENT)],
        &[&[IntrinsicDef::call(KEY, 1, |vm, _, roots, args, _| {
            vm.queue_microtask(roots, args)
        })]],
        None,
    );
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);

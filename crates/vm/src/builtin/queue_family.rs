use super::FUNCTION_PARENT;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};

const NAME: &str = "queueMicrotask";
const KEY: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
pub(crate) const OWNER: Owner = Owner::new(
    NAME,
    &[RootSpec::new(NAME, RootKind::Native(KEY), FUNCTION_PARENT)],
    &[&[IntrinsicDef::call(KEY, 1, |vm, _, _, args, _| {
        vm.queue_microtask(args)
    })]],
    None,
);

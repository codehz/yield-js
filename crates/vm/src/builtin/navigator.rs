//! `navigator`: host-supplied user agent and the compile-target platform.
//!
//! The user agent identifies the runtime to compatibility probes. It is not
//! the Fetch `User-Agent` header, which the host transport owns.

use super::globals::Owner;
use super::roots::{RootKind, RootSpec};
use crate::heap::Attributes;
use crate::value::Value;
use crate::vm::VmCore;

const ROOT: &str = "navigator";

/// Runtime identifier used when the host does not provide one. WinterTC
/// recommends a single HTTP product token rather than a browser-like string.
pub(crate) const DEFAULT_USER_AGENT: &str = "yield-js";

pub(crate) const ROOTS: &[RootSpec] = &[RootSpec::new(
    ROOT,
    RootKind::Plain,
    Some("object.prototype"),
)];

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const OWNER: Owner<P> = Owner::new(ROOT, ROOTS, &[], None);
}

/// `target_os` and `target_arch` name the platform this binary was compiled
/// for, not the host running it.
pub(crate) fn target_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub(crate) fn install(vm: &mut VmCore, user_agent: Option<&str>) {
    let navigator = vm.builtins.get(ROOT);
    let readonly = Attributes {
        writable: false,
        ..Attributes::ORDINARY
    };
    for (name, value) in [
        (
            "userAgent",
            Value::string(user_agent.unwrap_or(DEFAULT_USER_AGENT).into()),
        ),
        ("platform", Value::string(target_platform().into())),
    ] {
        vm.heap
            .define_data(navigator, name, value, readonly)
            .expect("navigator attribute");
    }
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);

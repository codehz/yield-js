//! Object URLs are capabilities rooted by a single VM, not a process registry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Heap, Object};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::VmCore;

const PREFIX: &str = "blob:null/";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ObjectUrls {
    entries: BTreeMap<String, ObjectId>,
    /// Monotonic name counter; restored names keep their embedded generation.
    next: u64,
}

impl ObjectUrls {
    pub(crate) fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.entries.values().copied()
    }

    pub(crate) fn resolve(&self, url: &Url) -> Option<ObjectId> {
        let key = url
            .as_str()
            .split_once('#')
            .map_or(url.as_str(), |(key, _)| key);
        self.entries.get(key).copied()
    }

    pub(crate) fn validate(&self, heap: &Heap) -> Result<(), VmError> {
        for (key, id) in &self.entries {
            let valid =
                key.strip_prefix(PREFIX).is_some_and(|suffix| {
                    let Some((generation, name)) = suffix.split_once('/') else {
                        return false;
                    };
                    !generation.is_empty()
                        && !name.is_empty()
                        && generation
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                }) && matches!(heap.get(*id), Ok(Object::Blob { .. } | Object::File { .. }));
            if !valid {
                return Err(VmError::InvalidSnapshot("invalid object URL entry".into()));
            }
        }
        Ok(())
    }
}

pub(crate) fn create(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    _: &Value,
) -> Result<Invocation, VmError> {
    let Some(ValueRef::Object(id)) = args.first().map(Value::view) else {
        return vm.throw_type_error(roots, "URL.createObjectURL requires a Blob");
    };
    if !matches!(vm.heap.get(id)?, Object::Blob { .. } | Object::File { .. }) {
        return vm.throw_type_error(roots, "URL.createObjectURL requires a Blob");
    }
    let generation = vm.epoch;
    let mut next = vm.object_urls.next;
    loop {
        let key = format!("{PREFIX}{generation:x}/{next:x}");
        next = next
            .checked_add(1)
            .ok_or_else(|| VmError::InvalidState("object URL name space exhausted".into()))?;
        if let std::collections::btree_map::Entry::Vacant(entry) = vm.object_urls.entries.entry(key)
        {
            let result = Value::string(entry.key().as_str().into());
            entry.insert(id);
            vm.object_urls.next = next;
            return Ok(Invocation::Value(result));
        }
    }
}

pub(crate) fn revoke(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    _: &Value,
) -> Result<Invocation, VmError> {
    let Some(input) = args.first() else {
        return vm.throw_type_error(roots, "URL.revokeObjectURL requires a URL");
    };
    let roots = &roots.with(input);
    let text = match vm.coerce_string(host, roots, input)? {
        Ok(text) => text,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    if let Ok(url) = Url::parse(&crate::builtin::url_api::usv_string(&text))
        && vm.object_urls.resolve(&url).is_some()
    {
        let key = url
            .as_str()
            .split_once('#')
            .map_or(url.as_str(), |(key, _)| key);
        vm.object_urls.entries.remove(key);
    }
    Ok(Invocation::Value(Value::UNDEFINED))
}

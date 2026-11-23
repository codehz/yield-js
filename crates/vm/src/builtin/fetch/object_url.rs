//! Object URLs are capabilities rooted by a single VM, not a process registry.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Heap, Object};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;

const PREFIX: &str = "blob:null/";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ObjectUrls {
    entries: BTreeMap<String, ObjectId>,
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
            if !key.strip_prefix(PREFIX).is_some_and(|suffix| {
                suffix.len() == 36
                    && suffix.bytes().enumerate().all(|(i, byte)| match i {
                        8 | 13 | 18 | 23 => byte == b'-',
                        14 => byte == b'4',
                        19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
                        _ => byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
                    })
            }) || !matches!(heap.get(*id), Ok(Object::Blob { .. } | Object::File { .. }))
            {
                return Err(VmError::InvalidSnapshot("invalid object URL entry".into()));
            }
        }
        Ok(())
    }
}

pub(crate) fn create(
    vm: &mut Vm,
    _: &mut dyn Host,
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
    let mut random = [0u8; 16];
    loop {
        if getrandom::fill(&mut random).is_err() {
            return vm.throw_type_error(roots, "Unable to allocate an object URL");
        }
        random[6] = (random[6] & 0x0f) | 0x40;
        random[8] = (random[8] & 0x3f) | 0x80;
        let mut key = String::with_capacity(PREFIX.len() + 36);
        key.push_str(PREFIX);
        for (index, byte) in random.into_iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                key.push('-');
            }
            write!(&mut key, "{byte:02x}").expect("write to String");
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = vm.object_urls.entries.entry(key)
        {
            let result = Value::string(entry.key().as_str().into());
            entry.insert(id);
            return Ok(Invocation::Value(result));
        }
    }
}

pub(crate) fn revoke(
    vm: &mut Vm,
    host: &mut dyn Host,
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

//! Read-only argument projections for hosts that opt into structural inspection.
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::heap::{Object, Property};
use crate::value::{HostValue, JsString, ObjectId, PropertyKey, Value};
use crate::{Vm, VmError};

/// Own enumerable data only. No getters, proxies, or heap IDs cross the boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostData {
    /// Stable opaque identity for `toBe`; not the VM object ID.
    pub identity: u64,
    pub root: usize,
    pub objects: BTreeMap<usize, HostDataObject>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HostDataSymbol {
    pub identity: u64,
    pub description: Option<JsString>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HostDataKey {
    String(JsString),
    Symbol(HostDataSymbol),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HostDataObject {
    Opaque {
        identity: u64,
    },
    Data {
        array_length: Option<usize>,
        properties: Vec<(HostDataKey, HostDataValue)>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HostDataValue {
    Object(usize),
    Scalar(HostValue),
    Symbol(HostDataSymbol),
    Accessor,
}

impl Vm {
    pub(crate) fn project_argument(&self, value: &Value) -> Result<HostValue, VmError> {
        let Value::Object(root) = *value else {
            return self.to_host_value(value);
        };
        if matches!(self.heap.get(root)?, Object::Native(_)) {
            return self.to_host_value(value);
        }
        let mut ids = BTreeMap::from([(root, 0usize)]);
        let mut pending = vec![root];
        let identity = self.project_object_identity(root);
        let mut objects = BTreeMap::new();
        while let Some(id) = pending.pop() {
            let projected_id = ids[&id];
            let object = self.heap.get(id)?;
            let array_length = match object {
                Object::Array(array) => Some(array.length),
                Object::Plain(_) => None,
                _ => {
                    objects.insert(
                        projected_id,
                        HostDataObject::Opaque {
                            identity: self.project_object_identity(id),
                        },
                    );
                    continue;
                }
            };
            let mut properties = Vec::new();
            for key in object.own_keys(false) {
                let Some(property) = object.own_property(key.borrowed()) else {
                    continue;
                };
                if !property.attributes().enumerable {
                    continue;
                }
                let key = match key {
                    PropertyKey::String(value) => HostDataKey::String(value),
                    PropertyKey::Symbol(id) => HostDataKey::Symbol(self.project_symbol(id)),
                };
                let projected = match property.into_owned() {
                    Property::Data(Value::Object(child)) => {
                        if matches!(self.heap.get(child)?, Object::Native(_)) {
                            HostDataValue::Scalar(self.to_host_value(&Value::Object(child))?)
                        } else {
                            let child_id = match ids.get(&child) {
                                Some(id) => *id,
                                None => {
                                    let id = ids.len();
                                    ids.insert(child, id);
                                    pending.push(child);
                                    id
                                }
                            };
                            HostDataValue::Object(child_id)
                        }
                    }
                    Property::Data(Value::Symbol(id)) => {
                        HostDataValue::Symbol(self.project_symbol(id))
                    }
                    Property::Data(value) => HostDataValue::Scalar(self.to_host_value(&value)?),
                    Property::Accessor { .. } => HostDataValue::Accessor,
                };
                properties.push((key, projected));
            }
            properties.sort_by(|a, b| a.0.cmp(&b.0));
            objects.insert(
                projected_id,
                HostDataObject::Data {
                    array_length,
                    properties,
                },
            );
        }
        Ok(HostValue::Data(HostData {
            identity,
            root: 0,
            objects,
        }))
    }
    fn project_symbol(&self, id: u32) -> HostDataSymbol {
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        self.build.build_id.hash(&mut identity);
        id.hash(&mut identity);
        HostDataSymbol {
            identity: identity.finish(),
            description: self.heap.symbols.get(&id).cloned().flatten(),
        }
    }
    fn project_object_identity(&self, id: ObjectId) -> u64 {
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        self.build.build_id.hash(&mut identity);
        id.hash(&mut identity);
        identity.finish()
    }
}

use std::collections::{BTreeMap, BTreeSet};

use crate::console::{
    ConsolePayload, ConsoleRow, ConsoleTable,
    preview::{self, MAX_PROPERTIES, Preview},
};
use crate::gc::ActiveRoots;
use crate::heap::Property;
use crate::host::Host;
use crate::value::{PropertyKey, ValueRef};
use crate::{JsString, Object, Value, Vm};

use super::logging::Completion;

pub(super) fn build(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    data: &Value,
    properties: Option<&Value>,
) -> Completion<Option<ConsolePayload>> {
    let mut columns = Vec::new();
    let mut known = BTreeSet::new();
    let mut truncated = false;
    let explicit = properties.filter(|value| !matches!(value.view(), ValueRef::Undefined));
    if let Some(properties) = explicit {
        if !matches!(properties.view(), ValueRef::Object(_)) {
            return Ok(Err(vm.type_error(
                roots,
                "console.table properties must be an iterable object",
            )?));
        }
        let (iterator, next) = match vm.get_iterator(host, roots, properties)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Err(error)),
        };
        let view = (&iterator, &next);
        let roots = &roots.with(&view);
        loop {
            let item = match vm.iterator_step(host, roots, &iterator, &next)? {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(error) => return Ok(Err(error)),
            };
            match vm.coerce_string(host, &roots.with(&item), &item)? {
                Ok(column) => {
                    if !known.contains(&column) {
                        if columns.len() < MAX_PROPERTIES {
                            known.insert(column.clone());
                            columns.push(column);
                        } else {
                            truncated = true;
                        }
                    }
                }
                Err(error) => {
                    vm.close_iterator(host, &roots.with(&error), &iterator)?;
                    return Ok(Err(error));
                }
            }
        }
    }
    let Some(object) = tabular_object(vm, data) else {
        return Ok(Ok(None));
    };
    let mut preview = Preview::new(vm);
    let mut rows = Vec::new();
    let value_column = JsString::from("Value");
    for (index, row) in object.enumerable_own_properties() {
        let PropertyKey::String(index) = index else {
            continue;
        };
        let Property::Data(row) = row.into_owned() else {
            continue;
        };
        if preview.remaining == 0 {
            truncated = true;
            break;
        }
        preview.remaining -= 1;
        let mut cells = BTreeMap::new();
        let mut add_cell = |key: JsString, value: &Value| {
            if explicit.is_some() && !known.contains(&key) {
                return true;
            }
            if preview.remaining == 0 {
                truncated = true;
                return false;
            }
            if explicit.is_none() && known.insert(key.clone()) {
                columns.push(key.clone());
            }
            preview.remaining -= 1;
            let value = preview.value(value, 0);
            truncated |= preview::is_truncated(&value);
            cells.insert(key, value);
            true
        };
        if let Some(row) = tabular_object(vm, &row) {
            for (key, property) in row.enumerable_own_properties() {
                if let (PropertyKey::String(key), Property::Data(value)) =
                    (key, property.into_owned())
                    && !add_cell(key, &value)
                {
                    break;
                }
            }
        } else {
            add_cell(value_column.clone(), &row);
        }
        rows.push((index, cells));
    }
    let rows = rows
        .into_iter()
        .map(|(index, mut cells)| ConsoleRow {
            index,
            cells: columns.iter().map(|key| cells.remove(key)).collect(),
        })
        .collect();
    Ok(Ok(Some(ConsolePayload::Table(ConsoleTable {
        columns,
        rows,
        truncated,
    }))))
}

fn tabular_object<'a>(vm: &'a Vm, value: &Value) -> Option<&'a Object> {
    let ValueRef::Object(id) = value.view() else {
        return None;
    };
    let object = vm.heap.get(id).expect("live console argument");
    matches!(preview::kind(object), "Array" | "Object").then_some(object)
}

use super::{FUNCTION_PARENT, define_builtin_data, link_constructor};
use crate::VmError;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, DescriptorUpdate, Object, PlainObject, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

const ROOT: &str = "domException";
const PROTOTYPE: &str = "domException.prototype";
const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("DOMException", Slot::Global, "");
const NAME: IntrinsicKey = IntrinsicKey::new("DOMException", Slot::PrototypeGetter, "name");
const MESSAGE: IntrinsicKey = IntrinsicKey::new("DOMException", Slot::PrototypeGetter, "message");
const CODE: IntrinsicKey = IntrinsicKey::new("DOMException", Slot::PrototypeGetter, "code");

// The position is the legacy code minus one. Some constants have no associated name.
const LEGACY_CODES: &[(&str, Option<&str>)] = &[
    ("INDEX_SIZE_ERR", Some("IndexSizeError")),
    ("DOMSTRING_SIZE_ERR", None),
    ("HIERARCHY_REQUEST_ERR", Some("HierarchyRequestError")),
    ("WRONG_DOCUMENT_ERR", Some("WrongDocumentError")),
    ("INVALID_CHARACTER_ERR", Some("InvalidCharacterError")),
    ("NO_DATA_ALLOWED_ERR", None),
    (
        "NO_MODIFICATION_ALLOWED_ERR",
        Some("NoModificationAllowedError"),
    ),
    ("NOT_FOUND_ERR", Some("NotFoundError")),
    ("NOT_SUPPORTED_ERR", Some("NotSupportedError")),
    ("INUSE_ATTRIBUTE_ERR", Some("InUseAttributeError")),
    ("INVALID_STATE_ERR", Some("InvalidStateError")),
    ("SYNTAX_ERR", Some("SyntaxError")),
    ("INVALID_MODIFICATION_ERR", Some("InvalidModificationError")),
    ("NAMESPACE_ERR", Some("NamespaceError")),
    ("INVALID_ACCESS_ERR", Some("InvalidAccessError")),
    ("VALIDATION_ERR", None),
    ("TYPE_MISMATCH_ERR", Some("TypeMismatchError")),
    ("SECURITY_ERR", Some("SecurityError")),
    ("NETWORK_ERR", Some("NetworkError")),
    ("ABORT_ERR", Some("AbortError")),
    ("URL_MISMATCH_ERR", Some("URLMismatchError")),
    ("QUOTA_EXCEEDED_ERR", Some("QuotaExceededError")),
    ("TIMEOUT_ERR", Some("TimeoutError")),
    ("INVALID_NODE_TYPE_ERR", Some("InvalidNodeTypeError")),
    ("DATA_CLONE_ERR", Some("DataCloneError")),
];

const ROOTS: &[RootSpec] = &[
    RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
        .fixed(PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
    RootSpec::new(PROTOTYPE, RootKind::Plain, Some("error.prototype")),
];

const INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor_with_target(
        CONSTRUCTOR,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "DOMException requires new"),
        construct,
    ),
    IntrinsicDef::call(NAME, 0, |vm, _, roots, _, receiver| {
        get(vm, roots, receiver, |name, _| Value::string(name.clone()))
    }),
    IntrinsicDef::call(MESSAGE, 0, |vm, _, roots, _, receiver| {
        get(vm, roots, receiver, |_, message| {
            Value::string(message.clone())
        })
    }),
    IntrinsicDef::call(CODE, 0, |vm, _, roots, _, receiver| {
        get(vm, roots, receiver, |name, _| {
            let code = LEGACY_CODES
                .iter()
                .position(|(_, legacy_name)| {
                    legacy_name.is_some_and(|legacy_name| name == legacy_name)
                })
                .map_or(0, |index| index + 1);
            Value::number(code as f64)
        })
    }),
];

pub(crate) const OWNER: Owner = Owner::new(ROOT, ROOTS, &[INTRINSICS], Some(install));

fn install(vm: &mut VmCore, _: &Value) {
    let constructor = vm.builtins.get(ROOT);
    let prototype = vm.builtins.get(PROTOTYPE);
    link_constructor(vm, constructor, prototype);
    for (key, intrinsic) in [("name", NAME), ("message", MESSAGE), ("code", CODE)] {
        let getter = vm.alloc_intrinsic(intrinsic);
        assert!(
            vm.heap
                .apply_descriptor(
                    prototype,
                    key.into(),
                    DescriptorUpdate {
                        get: Some(Some(getter)),
                        enumerable: Some(true),
                        configurable: Some(true),
                        ..DescriptorUpdate::default()
                    },
                )
                .expect("DOMException prototype getter")
        );
    }
    for (index, &(constant, _)) in LEGACY_CODES.iter().enumerate() {
        for target in [constructor, prototype] {
            define_builtin_data(
                vm,
                target,
                constant,
                Value::number((index + 1) as f64),
                Attributes {
                    enumerable: true,
                    ..Attributes::FIXED
                },
            );
        }
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("DOMException".into()),
        Attributes::FUNCTION_METADATA,
    );
}

fn get(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    read: impl FnOnce(&JsString, &JsString) -> Value,
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::DomException { name, message, .. } = vm.heap.get(id)?
    {
        return Ok(Invocation::Value(read(name, message)));
    }
    vm.throw_type_error(roots, "DOMException getter called on incompatible receiver")
}

fn construct(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    new_target: &Value,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let mut strings = [JsString::default(), JsString::from("Error")];
    for (text, arg) in strings.iter_mut().zip(&args) {
        if !matches!(arg.view(), ValueRef::Undefined) {
            *text = match vm.coerce_string(host, roots, arg)? {
                Ok(text) => text,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        }
    }
    let [message, name] = strings;
    let prototype =
        match vm.read_property(host, roots, new_target, PropertyKeyRef::String("prototype"))? {
            Invocation::Value(value) => match value.view() {
                ValueRef::Object(id) => Some(id),
                _ => None,
            },
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
    let value = if let Some(prototype) = prototype {
        vm.alloc_dom_exception_with(prototype, name, message)
    } else {
        vm.alloc_dom_exception(name, message)
    };
    Ok(Invocation::Value(value))
}

impl VmCore {
    /// Allocate from internal strings without consulting the global constructor or its properties.
    pub(crate) fn alloc_dom_exception(&mut self, name: JsString, message: JsString) -> Value {
        self.alloc_dom_exception_with(self.builtins.get(PROTOTYPE), name, message)
    }

    fn alloc_dom_exception_with(
        &mut self,
        prototype: ObjectId,
        name: JsString,
        message: JsString,
    ) -> Value {
        Value::object(self.heap.alloc(Object::DomException {
            object: PlainObject::new(Some(prototype)),
            name,
            message,
        }))
    }
}

//! Script TypeError / ReferenceError constructors for VM-raised errors.

use crate::error::ErrorKind;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::runtime::Invocation;
use crate::value::Value;
use crate::vm::VmCore;
impl VmCore {
    pub(crate) fn type_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Value, VmError> {
        self.alloc_class_error(roots, ErrorKind::TypeError.into(), Some(message.into()))
    }

    pub(crate) fn reference_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Value, VmError> {
        self.alloc_class_error(
            roots,
            ErrorKind::ReferenceError.into(),
            Some(message.into()),
        )
    }

    pub(crate) fn throw_type_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Invocation, VmError> {
        self.type_error(roots, message).map(Invocation::Thrown)
    }

    pub(crate) fn make_range_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Value, VmError> {
        self.alloc_class_error(roots, ErrorKind::RangeError.into(), Some(message.into()))
    }

    pub(crate) fn throw_range_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Invocation, VmError> {
        self.make_range_error(roots, message)
            .map(Invocation::Thrown)
    }
}

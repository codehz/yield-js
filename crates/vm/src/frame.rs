//! One activation's slots. Uninitialized and absent are the same state: `None`.
//! `undefined` is a real value and must not be used as a sentinel.

use crate::bytecode::{Function, Slot, SlotKind};
use crate::error::VmError;
use crate::value::{ObjectId, Value};

struct SlotFile {
    slots: Vec<Option<Value>>,
}

impl SlotFile {
    fn new(count: u16) -> Self {
        Self {
            slots: vec![None; count as usize],
        }
    }

    fn read(&self, index: u16, kind: SlotKind) -> Result<&Value, VmError> {
        match self.slots.get(index as usize) {
            Some(Some(value)) => Ok(value),
            Some(None) | None => Err(VmError::InvalidState(format!(
                "read of uninitialized {kind:?} slot {index}"
            ))),
        }
    }

    fn write(&mut self, index: u16, value: Value, kind: SlotKind) -> Result<(), VmError> {
        let Some(slot) = self.slots.get_mut(index as usize) else {
            return Err(VmError::InvalidState(format!(
                "write to out-of-range {kind:?} slot {index}"
            )));
        };
        *slot = Some(value);
        Ok(())
    }
}

pub(crate) struct Frame {
    pub(crate) function: usize,
    pub(crate) pc: usize,
    pub(crate) env: Option<ObjectId>,
    transient: SlotFile,
    persistent: SlotFile,
    pub(crate) async_promise: Option<ObjectId>,
}

impl Frame {
    pub(crate) fn new(
        function_index: usize,
        function: &Function,
        env: Option<ObjectId>,
        async_promise: Option<ObjectId>,
    ) -> Self {
        Self {
            function: function_index,
            pc: 0,
            env,
            transient: SlotFile::new(function.transient_slots),
            persistent: SlotFile::new(function.persistent_slots),
            async_promise,
        }
    }

    pub(crate) fn read(&self, slot: Slot) -> Result<Value, VmError> {
        self.read_ref(slot).cloned()
    }

    pub(crate) fn read_ref(&self, slot: Slot) -> Result<&Value, VmError> {
        self.file(slot.kind).read(slot.index, slot.kind)
    }

    pub(crate) fn write(&mut self, slot: Slot, value: Value) -> Result<(), VmError> {
        self.file_mut(slot.kind).write(slot.index, value, slot.kind)
    }

    pub(crate) fn read_list(&self, slots: &[Slot]) -> Result<Vec<Value>, VmError> {
        slots.iter().copied().map(|slot| self.read(slot)).collect()
    }

    pub(crate) fn read_saved(&self, slots: &[u16]) -> Result<Vec<Value>, VmError> {
        slots
            .iter()
            .map(|index| self.persistent.read(*index, SlotKind::Persistent).cloned())
            .collect()
    }

    fn file(&self, kind: SlotKind) -> &SlotFile {
        match kind {
            SlotKind::Transient => &self.transient,
            SlotKind::Persistent => &self.persistent,
        }
    }

    fn file_mut(&mut self, kind: SlotKind) -> &mut SlotFile {
        match kind {
            SlotKind::Transient => &mut self.transient,
            SlotKind::Persistent => &mut self.persistent,
        }
    }
}

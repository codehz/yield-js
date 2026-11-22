//! One activation's slots. Uninitialized and absent are the same state: `None`.
//! `undefined` is a real value and must not be used as a sentinel.

use crate::bytecode::{Function, Slot, SlotKind};
use crate::error::VmError;
use crate::gc::{TraceRoots, Tracer};
use crate::runtime::ContinuationTarget;
use crate::value::{ObjectId, Value};

pub(crate) struct Frame {
    pub(crate) function: usize,
    pub(crate) pc: usize,
    pub(crate) env: Option<ObjectId>,
    /// Payload of the generator `return` completion that rebuilt this frame.
    /// Consumed by `LoadGeneratorReturn`, so it never becomes a slot operand.
    pub(crate) generator_return: Option<Value>,
    /// Generator object this frame belongs to, when running a generator body.
    pub(crate) generator: Option<ObjectId>,
    /// Callee function object. Only generators read it, for `.prototype`.
    pub(crate) callee: Option<ObjectId>,
    // The two slot kinds share storage, but each has its own index space.
    slots: Vec<Option<Value>>,
    transient_count: usize,
    /// Where this activation delivers its result. `None` for synchronous
    /// activations and for a generator body that must not await.
    pub(crate) target: Option<ContinuationTarget>,
}

impl Frame {
    pub(crate) fn new(
        function_index: usize,
        function: &Function,
        env: Option<ObjectId>,
        target: Option<ContinuationTarget>,
    ) -> Self {
        let transient_count = usize::from(function.transient_slots);
        let slot_count = transient_count + usize::from(function.persistent_slots);
        if crate::perf::ENABLED {
            crate::perf::record(|counters| {
                counters.frame_allocations += 1;
                counters.frame_slots += slot_count as u64;
            });
        }
        Self {
            function: function_index,
            pc: 0,
            env,
            generator_return: None,
            generator: None,
            callee: None,
            slots: vec![None; slot_count],
            transient_count,
            target,
        }
    }

    pub(crate) fn read(&self, slot: Slot) -> Result<Value, VmError> {
        self.read_ref(slot).cloned()
    }

    pub(crate) fn read_ref(&self, slot: Slot) -> Result<&Value, VmError> {
        match self.file(slot.kind).get(usize::from(slot.index)) {
            Some(Some(value)) => Ok(value),
            _ => Err(VmError::InvalidState(format!(
                "read of uninitialized {:?} slot {}",
                slot.kind, slot.index
            ))),
        }
    }

    pub(crate) fn write(&mut self, slot: Slot, value: Value) -> Result<(), VmError> {
        let Some(dst) = self.file_mut(slot.kind).get_mut(usize::from(slot.index)) else {
            return Err(VmError::InvalidState(format!(
                "write to out-of-range {:?} slot {}",
                slot.kind, slot.index
            )));
        };
        *dst = Some(value);
        Ok(())
    }

    pub(crate) fn read_list(&self, slots: &[Slot]) -> Result<Vec<Value>, VmError> {
        slots.iter().copied().map(|slot| self.read(slot)).collect()
    }

    pub(crate) fn read_saved(&self, slots: &[u16]) -> Result<Vec<Value>, VmError> {
        slots
            .iter()
            .map(|index| self.read(Slot::persistent(*index)))
            .collect()
    }

    fn file(&self, kind: SlotKind) -> &[Option<Value>] {
        match kind {
            SlotKind::Transient => &self.slots[..self.transient_count],
            SlotKind::Persistent => &self.slots[self.transient_count..],
        }
    }

    fn file_mut(&mut self, kind: SlotKind) -> &mut [Option<Value>] {
        match kind {
            SlotKind::Transient => &mut self.slots[..self.transient_count],
            SlotKind::Persistent => &mut self.slots[self.transient_count..],
        }
    }
}

impl TraceRoots for Frame {
    fn frame(&self) -> Option<&Frame> {
        Some(self)
    }

    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        self.env.trace(tracer)?;
        self.target.map(ContinuationTarget::object).trace(tracer)?;
        self.generator_return.trace(tracer)?;
        self.generator.trace(tracer)?;
        self.callee.trace(tracer)?;
        for value in self.slots.iter().flatten() {
            value.trace(tracer)?;
        }
        Ok(())
    }
}

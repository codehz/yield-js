use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::bytecode::{BinaryOp, Build, Constant, Function, Instr, Slot, SlotKind, UnaryOp};
use crate::error::VmError;
use crate::heap::{NativeObject, Object, PromiseObject, PromiseState};
use crate::host::{Completion, Host, HostReply, PendingOperationMeta};
use crate::snapshot::{
    ContinuationRecord, ObjectRecord, PendingOperationRecord, SNAPSHOT_FORMAT_VERSION, Snapshot,
};
use crate::value::{ContinuationId, HostValue, NativeKey, ObjectId, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum PromiseStateView {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

#[derive(Debug, Clone)]
struct PendingOperation {
    meta: PendingOperationMeta,
    promise: ObjectId,
}

#[derive(Debug, Clone)]
struct Continuation {
    async_promise: ObjectId,
    func_id: u32,
    resume_id: u32,
    waiting_on: Option<ObjectId>,
    values: Vec<Value>,
}

#[derive(Debug, Clone)]
enum CompletionValue {
    Fulfilled(Value),
    Rejected(Value),
}

#[derive(Debug)]
enum Microtask {
    Resume {
        continuation: ContinuationId,
        completion: CompletionValue,
    },
}

struct Frame {
    function: usize,
    pc: usize,
    transient: Vec<Value>,
    transient_initialized: Vec<bool>,
    persistent: Vec<Value>,
    persistent_initialized: Vec<bool>,
    async_promise: Option<ObjectId>,
}

enum ExecOutcome {
    Return(Value),
    Suspended,
    Thrown(Value),
}

#[derive(Debug)]
pub struct Vm {
    build: Build,
    heap: BTreeMap<ObjectId, Object>,
    continuations: BTreeMap<ContinuationId, Continuation>,
    operations: BTreeMap<String, PendingOperation>,
    microtasks: VecDeque<Microtask>,
    globals: BTreeMap<String, Value>,
    next_object_id: ObjectId,
    next_continuation_id: ContinuationId,
}

impl Vm {
    pub fn new(build: Build, host: &dyn Host) -> Result<Self, VmError> {
        build.validate().map_err(VmError::InvalidBuild)?;
        Self::check_imports(&build, host)?;
        Ok(Self::empty(build))
    }

    fn empty(build: Build) -> Self {
        Self {
            build,
            heap: BTreeMap::new(),
            continuations: BTreeMap::new(),
            operations: BTreeMap::new(),
            microtasks: VecDeque::new(),
            globals: BTreeMap::new(),
            next_object_id: 1,
            next_continuation_id: 1,
        }
    }

    fn check_imports(build: &Build, host: &dyn Host) -> Result<(), VmError> {
        for import in &build.imports {
            if !host.has_import(&import.name) {
                return Err(VmError::MissingImport(import.name.clone()));
            }
        }
        Ok(())
    }

    pub fn build_id(&self) -> &str {
        &self.build.build_id
    }

    pub fn build(&self) -> &Build {
        &self.build
    }

    pub fn start(
        &mut self,
        host: &mut dyn Host,
        entry: &str,
        args: Vec<HostValue>,
    ) -> Result<Value, VmError> {
        let function = self
            .build
            .function_named(entry)
            .ok_or_else(|| VmError::InvalidBuild(format!("entry function {entry:?} not found")))?;
        let func_id = function.func_id;
        let args = args
            .iter()
            .map(|value| self.host_to_value(value))
            .collect::<Result<Vec<_>, _>>()?;
        self.call_function(host, func_id, args)
    }

    /// Runs queued VM microtasks. Host timer/event delivery belongs to the
    /// host; it should call this after completing an operation.
    pub fn run_until_idle(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
        while let Some(task) = self.microtasks.pop_front() {
            match task {
                Microtask::Resume {
                    continuation,
                    completion,
                } => self.resume_continuation(host, continuation, completion)?,
            }
        }
        Ok(())
    }

    pub fn complete_operation(
        &mut self,
        operation_id: &str,
        completion: Completion,
    ) -> Result<(), VmError> {
        if !self.operations.contains_key(operation_id) {
            return Err(VmError::UnknownOperation(operation_id.to_owned()));
        }
        let value = match &completion {
            Completion::Fulfilled(value) | Completion::Rejected(value) => {
                self.host_to_value(value)?
            }
        };
        let operation = self
            .operations
            .remove(operation_id)
            .ok_or_else(|| VmError::UnknownOperation(operation_id.to_owned()))?;
        let completion = match completion {
            Completion::Fulfilled(_) => CompletionValue::Fulfilled(value),
            Completion::Rejected(_) => CompletionValue::Rejected(value),
        };
        self.settle_promise(operation.promise, completion)
    }

    pub fn pending_operation_ids(&self) -> Vec<String> {
        self.operations.keys().cloned().collect()
    }

    pub fn promise_state(&self, value: &Value) -> Result<PromiseStateView, VmError> {
        let Value::Object(id) = value else {
            return Err(VmError::InvalidState("value is not an object".to_owned()));
        };
        let object = self
            .heap
            .get(id)
            .ok_or_else(|| VmError::InvalidState(format!("object {id} does not exist")))?;
        let promise = object
            .as_promise()
            .ok_or_else(|| VmError::InvalidState(format!("object {id} is not a Promise")))?;
        Ok(match &promise.state {
            PromiseState::Pending => PromiseStateView::Pending,
            PromiseState::Fulfilled(value) => PromiseStateView::Fulfilled(value.clone()),
            PromiseState::Rejected(value) => PromiseStateView::Rejected(value.clone()),
        })
    }

    pub fn to_host_value(&self, value: &Value) -> Result<HostValue, VmError> {
        match value {
            Value::Undefined => Ok(HostValue::Undefined),
            Value::Null => Ok(HostValue::Null),
            Value::Bool(value) => Ok(HostValue::Bool(*value)),
            Value::Number(value) => Ok(HostValue::Number(*value)),
            Value::String(value) => Ok(HostValue::String(value.clone())),
            Value::Object(id) => match self.heap.get(id) {
                Some(Object::Native(native)) => Ok(HostValue::Native(native.key.clone())),
                Some(_) => Err(VmError::InvalidState(format!(
                    "object {id} cannot cross the host boundary"
                ))),
                None => Err(VmError::InvalidState(format!("object {id} does not exist"))),
            },
        }
    }

    fn host_to_value(&mut self, value: &HostValue) -> Result<Value, VmError> {
        match value {
            HostValue::Undefined => Ok(Value::Undefined),
            HostValue::Null => Ok(Value::Null),
            HostValue::Bool(value) => Ok(Value::Bool(*value)),
            HostValue::Number(value) => {
                if !value.is_finite() {
                    return Err(VmError::Host(
                        "non-finite numbers are not supported by this PoC".to_owned(),
                    ));
                }
                Ok(Value::Number(*value))
            }
            HostValue::String(value) => Ok(Value::String(value.clone())),
            HostValue::Native(key) => Ok(Value::Object(self.intern_native(key.clone()))),
        }
    }

    fn intern_native(&mut self, key: NativeKey) -> ObjectId {
        if let Some((id, _)) = self
            .heap
            .iter()
            .find(|(_, object)| matches!(object, Object::Native(native) if native.key == key))
        {
            return *id;
        }
        self.alloc_object(Object::Native(NativeObject { key }))
    }

    fn alloc_object(&mut self, object: Object) -> ObjectId {
        let id = self.next_object_id;
        self.next_object_id = self
            .next_object_id
            .checked_add(1)
            .expect("object id space exhausted");
        self.heap.insert(id, object);
        id
    }

    fn create_promise(&mut self) -> ObjectId {
        self.alloc_object(Object::Promise(PromiseObject {
            state: PromiseState::Pending,
            reactions: Vec::new(),
        }))
    }

    fn next_continuation_id(&mut self) -> ContinuationId {
        let id = self.next_continuation_id;
        self.next_continuation_id = self
            .next_continuation_id
            .checked_add(1)
            .expect("continuation id space exhausted");
        id
    }

    fn call_function(
        &mut self,
        host: &mut dyn Host,
        func_id: u32,
        args: Vec<Value>,
    ) -> Result<Value, VmError> {
        let function_index = self
            .build
            .functions
            .iter()
            .position(|function| function.func_id == func_id)
            .ok_or_else(|| VmError::InvalidState(format!("function {func_id} does not exist")))?;
        let is_async = self.build.functions[function_index].is_async;
        let async_promise = is_async.then(|| self.create_promise());
        let function = &self.build.functions[function_index];

        let mut frame = Frame::new(function_index, function, async_promise);
        for (index, slot) in function.param_slots.iter().copied().enumerate() {
            let value = args.get(index).cloned().unwrap_or(Value::Undefined);
            write_slot(&mut frame, slot, value)?;
        }

        let outcome = self.execute_frame(host, frame)?;
        match outcome {
            ExecOutcome::Suspended => {
                let promise = async_promise.ok_or_else(|| {
                    VmError::InvalidState("synchronous function suspended".to_owned())
                })?;
                Ok(Value::Object(promise))
            }
            ExecOutcome::Return(value) => {
                if let Some(promise) = async_promise {
                    self.settle_promise(promise, CompletionValue::Fulfilled(value))?;
                    Ok(Value::Object(promise))
                } else {
                    Ok(value)
                }
            }
            ExecOutcome::Thrown(value) => {
                if let Some(promise) = async_promise {
                    self.settle_promise(promise, CompletionValue::Rejected(value))?;
                    Ok(Value::Object(promise))
                } else {
                    Err(VmError::Thrown(value))
                }
            }
        }
    }

    fn execute_frame(
        &mut self,
        host: &mut dyn Host,
        mut frame: Frame,
    ) -> Result<ExecOutcome, VmError> {
        loop {
            let function = &self.build.functions[frame.function];
            if frame.pc >= function.code.len() {
                return Ok(ExecOutcome::Return(Value::Undefined));
            }
            let instr = function.code[frame.pc].clone();
            frame.pc += 1;

            match instr {
                Instr::Const { dst, constant } => {
                    let value = self.constant_value(constant)?;
                    write_slot(&mut frame, dst, value)?;
                }
                Instr::Undefined { dst } => write_slot(&mut frame, dst, Value::Undefined)?,
                Instr::Move { dst, src } => {
                    let value = read_slot(&frame, src)?;
                    write_slot(&mut frame, dst, value)?;
                }
                Instr::Binary { dst, op, lhs, rhs } => {
                    let lhs = read_slot(&frame, lhs)?;
                    let rhs = read_slot(&frame, rhs)?;
                    let value = binary_op(op, lhs, rhs)?;
                    write_slot(&mut frame, dst, value)?;
                }
                Instr::Unary { dst, op, src } => {
                    let src = read_slot(&frame, src)?;
                    write_slot(&mut frame, dst, unary_op(op, src))?;
                }
                Instr::CallImport { dst, import, args } => {
                    let import_name = self
                        .build
                        .imports
                        .get(import as usize)
                        .ok_or_else(|| {
                            VmError::InvalidState(format!("import {import} does not exist"))
                        })?
                        .name
                        .clone();
                    let mut host_args = Vec::with_capacity(args.len());
                    for arg in args {
                        host_args.push(self.to_host_value(&read_slot(&frame, arg)?)?);
                    }
                    let reply = host.call(&import_name, &host_args).map_err(VmError::Host)?;
                    let value = match reply {
                        HostReply::Value(value) => self.host_to_value(&value)?,
                        HostReply::Operation { operation_id } => {
                            if operation_id.is_empty() {
                                return Err(VmError::Host(
                                    "host operation id must not be empty".to_owned(),
                                ));
                            }
                            if self.operations.contains_key(&operation_id) {
                                return Err(VmError::DuplicateOperation(operation_id));
                            }
                            let promise = self.create_promise();
                            self.operations.insert(
                                operation_id.clone(),
                                PendingOperation {
                                    meta: PendingOperationMeta {
                                        id: operation_id,
                                        import: import_name,
                                    },
                                    promise,
                                },
                            );
                            Value::Object(promise)
                        }
                    };
                    write_slot(&mut frame, dst, value)?;
                }
                Instr::CallFunction {
                    dst,
                    function: callee,
                    args,
                } => {
                    let mut values = Vec::with_capacity(args.len());
                    for arg in args {
                        values.push(read_slot(&frame, arg)?);
                    }
                    let value = match self.call_function(host, callee, values) {
                        Ok(value) => value,
                        Err(VmError::Thrown(value)) => return Ok(ExecOutcome::Thrown(value)),
                        Err(error) => return Err(error),
                    };
                    write_slot(&mut frame, dst, value)?;
                }
                Instr::Await { dst, input, resume } => {
                    let input_value = read_slot(&frame, input)?;
                    let async_promise = frame.async_promise.ok_or_else(|| {
                        VmError::InvalidState("Await outside an async frame".to_owned())
                    })?;
                    let resume_entry = self
                        .build
                        .functions
                        .get(frame.function)
                        .and_then(|function| function.resumes.get(resume as usize))
                        .cloned()
                        .ok_or_else(|| {
                            VmError::InvalidState(format!("resume {resume} does not exist"))
                        })?;
                    let waiting_on = match &input_value {
                        Value::Object(id) => {
                            let object = self.heap.get(id).ok_or_else(|| {
                                VmError::InvalidState(format!("object {id} does not exist"))
                            })?;
                            if object.as_promise().is_none() {
                                return Err(VmError::InvalidState(
                                    "await only supports VM promises and plain values in this PoC"
                                        .to_owned(),
                                ));
                            }
                            Some(*id)
                        }
                        _ => None,
                    };
                    let mut values = Vec::with_capacity(resume_entry.saved_slots.len());
                    for slot in &resume_entry.saved_slots {
                        values.push(read_slot(
                            &frame,
                            Slot {
                                kind: SlotKind::Persistent,
                                index: *slot,
                            },
                        )?);
                    }
                    let continuation_id = self.next_continuation_id();
                    self.continuations.insert(
                        continuation_id,
                        Continuation {
                            async_promise,
                            func_id: self.build.functions[frame.function].func_id,
                            resume_id: resume,
                            waiting_on,
                            values,
                        },
                    );

                    match waiting_on {
                        Some(promise) => {
                            let state = self.promise_state_for(promise)?.clone();
                            match state {
                                PromiseState::Pending => {
                                    self.promise_mut(promise)?.reactions.push(continuation_id);
                                }
                                PromiseState::Fulfilled(value) => {
                                    self.microtasks.push_back(Microtask::Resume {
                                        continuation: continuation_id,
                                        completion: CompletionValue::Fulfilled(value),
                                    });
                                }
                                PromiseState::Rejected(value) => {
                                    self.microtasks.push_back(Microtask::Resume {
                                        continuation: continuation_id,
                                        completion: CompletionValue::Rejected(value),
                                    });
                                }
                            }
                        }
                        None => {
                            self.microtasks.push_back(Microtask::Resume {
                                continuation: continuation_id,
                                completion: CompletionValue::Fulfilled(input_value),
                            });
                        }
                    }
                    let _ = dst;
                    return Ok(ExecOutcome::Suspended);
                }
                Instr::Jump { target } => frame.pc = target as usize,
                Instr::BranchFalse { cond, target } => {
                    if !read_slot(&frame, cond)?.truthy() {
                        frame.pc = target as usize;
                    }
                }
                Instr::Return { src } => return Ok(ExecOutcome::Return(read_slot(&frame, src)?)),
                Instr::Throw { src } => return Ok(ExecOutcome::Thrown(read_slot(&frame, src)?)),
            }
        }
    }

    fn constant_value(&self, index: u32) -> Result<Value, VmError> {
        let constant = self
            .build
            .constants
            .get(index as usize)
            .ok_or_else(|| VmError::InvalidState(format!("constant {index} does not exist")))?;
        Ok(match constant {
            Constant::Null => Value::Null,
            Constant::Bool(value) => Value::Bool(*value),
            Constant::Number(value) => Value::Number(*value),
            Constant::String(value) => Value::String(value.clone()),
        })
    }

    fn promise_state_for(&self, id: ObjectId) -> Result<&PromiseState, VmError> {
        self.heap
            .get(&id)
            .and_then(Object::as_promise)
            .map(|promise| &promise.state)
            .ok_or_else(|| VmError::InvalidState(format!("object {id} is not a Promise")))
    }

    fn promise_mut(&mut self, id: ObjectId) -> Result<&mut PromiseObject, VmError> {
        self.heap
            .get_mut(&id)
            .and_then(Object::as_promise_mut)
            .ok_or_else(|| VmError::InvalidState(format!("object {id} is not a Promise")))
    }

    fn settle_promise(
        &mut self,
        promise: ObjectId,
        completion: CompletionValue,
    ) -> Result<(), VmError> {
        let reactions = {
            let promise_object = self.promise_mut(promise)?;
            if !matches!(promise_object.state, PromiseState::Pending) {
                return Err(VmError::InvalidState(format!(
                    "Promise {promise} is already settled"
                )));
            }
            promise_object.state = match &completion {
                CompletionValue::Fulfilled(value) => PromiseState::Fulfilled(value.clone()),
                CompletionValue::Rejected(value) => PromiseState::Rejected(value.clone()),
            };
            std::mem::take(&mut promise_object.reactions)
        };
        for continuation in reactions {
            self.microtasks.push_back(Microtask::Resume {
                continuation,
                completion: completion.clone(),
            });
        }
        Ok(())
    }

    fn resume_continuation(
        &mut self,
        host: &mut dyn Host,
        continuation_id: ContinuationId,
        completion: CompletionValue,
    ) -> Result<(), VmError> {
        let continuation = self.continuations.remove(&continuation_id).ok_or_else(|| {
            VmError::InvalidState(format!("continuation {continuation_id} does not exist"))
        })?;
        let function_index = self
            .build
            .functions
            .iter()
            .position(|function| function.func_id == continuation.func_id)
            .ok_or_else(|| {
                VmError::InvalidState(format!("function {} does not exist", continuation.func_id))
            })?;
        let function = &self.build.functions[function_index];
        let resume_entry = function
            .resumes
            .get(continuation.resume_id as usize)
            .cloned()
            .ok_or_else(|| {
                VmError::InvalidState(format!("resume {} does not exist", continuation.resume_id))
            })?;
        if resume_entry.saved_slots.len() != continuation.values.len() {
            return Err(VmError::InvalidState(format!(
                "continuation {continuation_id} has {} values, expected {}",
                continuation.values.len(),
                resume_entry.saved_slots.len()
            )));
        }

        let mut frame = Frame::new(function_index, function, Some(continuation.async_promise));
        for (slot, value) in resume_entry
            .saved_slots
            .iter()
            .copied()
            .zip(continuation.values)
        {
            write_slot(
                &mut frame,
                Slot {
                    kind: SlotKind::Persistent,
                    index: slot,
                },
                value,
            )?;
        }
        match completion {
            CompletionValue::Fulfilled(value) => {
                write_slot(&mut frame, resume_entry.fulfill_dst, value)?;
                frame.pc = resume_entry.fulfill_pc as usize;
            }
            CompletionValue::Rejected(value) => {
                write_slot(&mut frame, resume_entry.reject_dst, value)?;
                frame.pc = resume_entry.reject_pc as usize;
            }
        }

        let outcome = self.execute_frame(host, frame)?;
        match outcome {
            ExecOutcome::Suspended => Ok(()),
            ExecOutcome::Return(value) => self.settle_promise(
                continuation.async_promise,
                CompletionValue::Fulfilled(value),
            ),
            ExecOutcome::Thrown(value) => {
                self.settle_promise(continuation.async_promise, CompletionValue::Rejected(value))
            }
        }
    }

    pub fn snapshot(&self, roots: &[Value]) -> Result<Snapshot, VmError> {
        if !self.microtasks.is_empty() {
            return Err(VmError::InvalidState(
                "snapshot requires a drained microtask queue".to_owned(),
            ));
        }

        let mut snapshot = Snapshot::new(self.build.build_id.clone());
        snapshot.globals = self.globals.clone();

        let mut objects = BTreeSet::new();
        let mut continuations = BTreeSet::new();
        let mut object_queue = VecDeque::new();
        let mut continuation_queue = VecDeque::new();

        let mark_value = |value: &Value,
                          objects: &mut BTreeSet<ObjectId>,
                          object_queue: &mut VecDeque<ObjectId>|
         -> Result<(), VmError> {
            if let Value::Object(id) = value {
                if !self.heap.contains_key(id) {
                    return Err(VmError::InvalidState(format!(
                        "snapshot root references missing object {id}"
                    )));
                }
                if objects.insert(*id) {
                    object_queue.push_back(*id);
                }
            }
            Ok(())
        };

        for root in roots {
            mark_value(root, &mut objects, &mut object_queue)?;
        }
        for value in self.globals.values() {
            mark_value(value, &mut objects, &mut object_queue)?;
        }
        for operation in self.operations.values() {
            mark_value(
                &Value::Object(operation.promise),
                &mut objects,
                &mut object_queue,
            )?;
        }

        loop {
            let mut progressed = false;
            while let Some(id) = object_queue.pop_front() {
                progressed = true;
                let object = self
                    .heap
                    .get(&id)
                    .ok_or_else(|| VmError::InvalidState(format!("object {id} does not exist")))?;
                match object {
                    Object::Promise(promise) => {
                        for continuation in &promise.reactions {
                            if !self.continuations.contains_key(continuation) {
                                return Err(VmError::InvalidState(format!(
                                    "Promise {id} references missing continuation {continuation}"
                                )));
                            }
                            if continuations.insert(*continuation) {
                                continuation_queue.push_back(*continuation);
                            }
                        }
                    }
                    Object::Native(_) => {}
                    Object::Plain(plain) => {
                        for value in plain.props.values() {
                            mark_value(value, &mut objects, &mut object_queue)?;
                        }
                    }
                }
            }
            while let Some(id) = continuation_queue.pop_front() {
                progressed = true;
                let continuation = self.continuations.get(&id).ok_or_else(|| {
                    VmError::InvalidState(format!("continuation {id} does not exist"))
                })?;
                mark_value(
                    &Value::Object(continuation.async_promise),
                    &mut objects,
                    &mut object_queue,
                )?;
                if let Some(waiting_on) = continuation.waiting_on {
                    mark_value(&Value::Object(waiting_on), &mut objects, &mut object_queue)?;
                }
                for value in &continuation.values {
                    mark_value(value, &mut objects, &mut object_queue)?;
                }
            }
            if !progressed {
                break;
            }
        }

        for id in objects {
            let object = self
                .heap
                .get(&id)
                .cloned()
                .ok_or_else(|| VmError::InvalidState(format!("object {id} does not exist")))?;
            snapshot.objects.push(ObjectRecord { id, object });
        }
        for id in continuations {
            let continuation = self.continuations.get(&id).ok_or_else(|| {
                VmError::InvalidState(format!("continuation {id} does not exist"))
            })?;
            let waiting_on = continuation.waiting_on.ok_or_else(|| {
                VmError::InvalidState(format!(
                    "continuation {id} waits on a plain value and cannot be snapshotted"
                ))
            })?;
            snapshot.continuations.push(ContinuationRecord {
                id,
                async_promise: continuation.async_promise,
                func_id: continuation.func_id,
                resume_id: continuation.resume_id,
                waiting_on,
                values: continuation.values.clone(),
            });
        }
        for operation in self.operations.values() {
            snapshot.operations.push(PendingOperationRecord {
                id: operation.meta.id.clone(),
                import: operation.meta.import.clone(),
                promise: operation.promise,
            });
        }
        Ok(snapshot)
    }

    pub fn restore(snapshot: Snapshot, build: Build, host: &mut dyn Host) -> Result<Self, VmError> {
        if snapshot.format_version != SNAPSHOT_FORMAT_VERSION {
            return Err(VmError::InvalidSnapshot(format!(
                "unsupported snapshot format {}, expected {}",
                snapshot.format_version, SNAPSHOT_FORMAT_VERSION
            )));
        }
        if snapshot.bytecode_version != crate::bytecode::BYTECODE_VERSION {
            return Err(VmError::InvalidSnapshot(format!(
                "unsupported bytecode version {}, expected {}",
                snapshot.bytecode_version,
                crate::bytecode::BYTECODE_VERSION
            )));
        }
        build.validate().map_err(VmError::InvalidBuild)?;
        if snapshot.build_id != build.build_id {
            return Err(VmError::InvalidSnapshot(format!(
                "build id mismatch: snapshot {}, build {}",
                snapshot.build_id, build.build_id
            )));
        }
        Self::check_imports(&build, host)?;

        let mut heap = BTreeMap::new();
        for record in &snapshot.objects {
            if heap.insert(record.id, record.object.clone()).is_some() {
                return Err(VmError::InvalidSnapshot(format!(
                    "duplicate object id {}",
                    record.id
                )));
            }
        }

        let mut value_refs = Vec::new();
        for record in &snapshot.objects {
            match &record.object {
                Object::Promise(promise) => {
                    if let PromiseState::Fulfilled(value) | PromiseState::Rejected(value) =
                        &promise.state
                    {
                        value_refs.push(value);
                    }
                }
                Object::Native(native) => {
                    if !host.has_native(&native.key) {
                        return Err(VmError::MissingNative(native.key.clone()));
                    }
                }
                Object::Plain(plain) => value_refs.extend(plain.props.values()),
            }
        }
        for value in snapshot.globals.values() {
            value_refs.push(value);
        }
        for record in &snapshot.continuations {
            value_refs.extend(record.values.iter());
        }
        for value in value_refs {
            if let Value::Object(id) = value
                && !heap.contains_key(id)
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "value references missing object {id}"
                )));
            }
        }

        let mut continuation_records = BTreeMap::new();
        for record in &snapshot.continuations {
            if continuation_records
                .insert(record.id, record.clone())
                .is_some()
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "duplicate continuation id {}",
                    record.id
                )));
            }
            let function = build.function(record.func_id).ok_or_else(|| {
                VmError::InvalidSnapshot(format!(
                    "continuation {} references missing function {}",
                    record.id, record.func_id
                ))
            })?;
            let resume = function
                .resumes
                .get(record.resume_id as usize)
                .ok_or_else(|| {
                    VmError::InvalidSnapshot(format!(
                        "continuation {} references missing resume {}",
                        record.id, record.resume_id
                    ))
                })?;
            if resume.saved_slots.len() != record.values.len() {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} has {} values, expected {}",
                    record.id,
                    record.values.len(),
                    resume.saved_slots.len()
                )));
            }
            match heap.get(&record.async_promise) {
                Some(Object::Promise(promise))
                    if matches!(promise.state, PromiseState::Pending) => {}
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "continuation {} async result {} is not a pending Promise",
                        record.id, record.async_promise
                    )));
                }
            }
            let waiting = match heap.get(&record.waiting_on) {
                Some(Object::Promise(promise)) => promise,
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "continuation {} waits on non-Promise object {}",
                        record.id, record.waiting_on
                    )));
                }
            };
            if !matches!(waiting.state, PromiseState::Pending) {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} waits on a settled Promise",
                    record.id
                )));
            }
            let occurrences = waiting
                .reactions
                .iter()
                .filter(|continuation| **continuation == record.id)
                .count();
            if occurrences != 1 {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} must appear exactly once in Promise {} reactions, found {occurrences}",
                    record.id, record.waiting_on
                )));
            }
        }

        for (id, object) in &heap {
            if let Object::Promise(promise) = object {
                for continuation in &promise.reactions {
                    let Some(record) = continuation_records.get(continuation) else {
                        return Err(VmError::InvalidSnapshot(format!(
                            "Promise {id} references missing continuation {continuation}"
                        )));
                    };
                    if record.waiting_on != *id {
                        return Err(VmError::InvalidSnapshot(format!(
                            "Promise {id} reaction {continuation} waits on {}",
                            record.waiting_on
                        )));
                    }
                }
            }
        }

        let mut operations = BTreeMap::new();
        let mut operation_promises = BTreeSet::new();
        for record in &snapshot.operations {
            if operations.contains_key(&record.id) {
                return Err(VmError::InvalidSnapshot(format!(
                    "duplicate host operation {:?}",
                    record.id
                )));
            }
            if !build
                .imports
                .iter()
                .any(|import| import.name == record.import)
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "operation {:?} references missing import {:?}",
                    record.id, record.import
                )));
            }
            match heap.get(&record.promise) {
                Some(Object::Promise(promise))
                    if matches!(promise.state, PromiseState::Pending) => {}
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "operation {:?} references non-pending Promise {}",
                        record.id, record.promise
                    )));
                }
            }
            if !operation_promises.insert(record.promise) {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {} is associated with more than one host operation",
                    record.promise
                )));
            }
            let meta = PendingOperationMeta {
                id: record.id.clone(),
                import: record.import.clone(),
            };
            if !host.rebuild_operation(&meta) {
                return Err(VmError::MissingOperation(record.id.clone()));
            }
            operations.insert(
                record.id.clone(),
                PendingOperation {
                    meta,
                    promise: record.promise,
                },
            );
        }

        let continuations = continuation_records
            .into_values()
            .map(|record| {
                (
                    record.id,
                    Continuation {
                        async_promise: record.async_promise,
                        func_id: record.func_id,
                        resume_id: record.resume_id,
                        waiting_on: Some(record.waiting_on),
                        values: record.values,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let next_object_id = heap.keys().next_back().map_or(1, |id| id + 1);
        let next_continuation_id = continuations.keys().next_back().map_or(1, |id| id + 1);
        Ok(Self {
            build,
            heap,
            continuations,
            operations,
            microtasks: VecDeque::new(),
            globals: snapshot.globals,
            next_object_id,
            next_continuation_id,
        })
    }
}

impl Frame {
    fn new(function_index: usize, function: &Function, async_promise: Option<ObjectId>) -> Self {
        Self {
            function: function_index,
            pc: 0,
            transient: vec![Value::Undefined; function.transient_slots as usize],
            transient_initialized: vec![false; function.transient_slots as usize],
            persistent: vec![Value::Undefined; function.persistent_slots as usize],
            persistent_initialized: vec![false; function.persistent_slots as usize],
            async_promise,
        }
    }
}

fn read_slot(frame: &Frame, slot: Slot) -> Result<Value, VmError> {
    let (values, initialized) = match slot.kind {
        SlotKind::Transient => (&frame.transient, &frame.transient_initialized),
        SlotKind::Persistent => (&frame.persistent, &frame.persistent_initialized),
    };
    let index = slot.index as usize;
    if !initialized.get(index).copied().unwrap_or(false) {
        return Err(VmError::InvalidState(format!(
            "read of uninitialized {:?} slot {}",
            slot.kind, slot.index
        )));
    }
    Ok(values[index].clone())
}

fn write_slot(frame: &mut Frame, slot: Slot, value: Value) -> Result<(), VmError> {
    let (values, initialized) = match slot.kind {
        SlotKind::Transient => (&mut frame.transient, &mut frame.transient_initialized),
        SlotKind::Persistent => (&mut frame.persistent, &mut frame.persistent_initialized),
    };
    let index = slot.index as usize;
    if index >= values.len() {
        return Err(VmError::InvalidState(format!(
            "write to out-of-range {:?} slot {}",
            slot.kind, slot.index
        )));
    }
    values[index] = value;
    initialized[index] = true;
    Ok(())
}

fn binary_op(operator: BinaryOp, lhs: Value, rhs: Value) -> Result<Value, VmError> {
    Ok(match operator {
        BinaryOp::Add => match (lhs, rhs) {
            (Value::String(lhs), rhs) => Value::String(lhs + &to_js_string(&rhs)),
            (lhs, Value::String(rhs)) => Value::String(to_js_string(&lhs) + &rhs),
            (lhs, rhs) => Value::Number(to_number(&lhs)? + to_number(&rhs)?),
        },
        BinaryOp::Sub => Value::Number(to_number(&lhs)? - to_number(&rhs)?),
        BinaryOp::Mul => Value::Number(to_number(&lhs)? * to_number(&rhs)?),
        BinaryOp::Div => Value::Number(to_number(&lhs)? / to_number(&rhs)?),
        BinaryOp::Rem => Value::Number(to_number(&lhs)? % to_number(&rhs)?),
        BinaryOp::Exp => Value::Number(to_number(&lhs)?.powf(to_number(&rhs)?)),
        BinaryOp::Lt => compare(&lhs, &rhs, |ordering| ordering.is_lt()),
        BinaryOp::Le => compare(&lhs, &rhs, |ordering| ordering.is_le()),
        BinaryOp::Gt => compare(&lhs, &rhs, |ordering| ordering.is_gt()),
        BinaryOp::Ge => compare(&lhs, &rhs, |ordering| ordering.is_ge()),
        BinaryOp::Eq => Value::Bool(loose_equal(&lhs, &rhs)?),
        BinaryOp::Ne => Value::Bool(!loose_equal(&lhs, &rhs)?),
        BinaryOp::StrictEq => Value::Bool(strict_equal(&lhs, &rhs)),
        BinaryOp::StrictNe => Value::Bool(!strict_equal(&lhs, &rhs)),
        BinaryOp::BitAnd => Value::Number((to_int32(&lhs)? & to_int32(&rhs)?) as f64),
        BinaryOp::BitOr => Value::Number((to_int32(&lhs)? | to_int32(&rhs)?) as f64),
        BinaryOp::BitXor => Value::Number((to_int32(&lhs)? ^ to_int32(&rhs)?) as f64),
        BinaryOp::Shl => {
            Value::Number((to_int32(&lhs)?).wrapping_shl(to_uint32(&rhs)? & 0x1f) as f64)
        }
        BinaryOp::Shr => {
            Value::Number((to_int32(&lhs)?).wrapping_shr(to_uint32(&rhs)? & 0x1f) as f64)
        }
        BinaryOp::UShr => {
            Value::Number((to_uint32(&lhs)?).wrapping_shr(to_uint32(&rhs)? & 0x1f) as f64)
        }
    })
}

fn unary_op(operator: UnaryOp, value: Value) -> Value {
    match operator {
        UnaryOp::Neg => Value::Number(-to_number_lossy(&value)),
        UnaryOp::Pos => Value::Number(to_number_lossy(&value)),
        UnaryOp::Not => Value::Bool(!value.truthy()),
        UnaryOp::BitNot => Value::Number(!(to_int32_lossy(&value)) as f64),
        UnaryOp::TypeOf => Value::String(value.type_of().to_owned()),
        UnaryOp::Void => Value::Undefined,
    }
}

fn compare(lhs: &Value, rhs: &Value, predicate: impl Fn(std::cmp::Ordering) -> bool) -> Value {
    if let (Value::String(lhs), Value::String(rhs)) = (lhs, rhs) {
        return Value::Bool(predicate(lhs.cmp(rhs)));
    }
    let lhs = to_number_lossy(lhs);
    let rhs = to_number_lossy(rhs);
    Value::Bool(lhs.partial_cmp(&rhs).is_some_and(predicate))
}

fn strict_equal(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(lhs), Value::Bool(rhs)) => lhs == rhs,
        (Value::Number(lhs), Value::Number(rhs)) => lhs == rhs,
        (Value::String(lhs), Value::String(rhs)) => lhs == rhs,
        (Value::Object(lhs), Value::Object(rhs)) => lhs == rhs,
        _ => false,
    }
}

fn loose_equal(lhs: &Value, rhs: &Value) -> Result<bool, VmError> {
    if strict_equal(lhs, rhs) {
        return Ok(true);
    }
    match (lhs, rhs) {
        (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => Ok(true),
        (Value::Number(_), Value::String(_)) => Ok(to_number(lhs)? == to_number(rhs)?),
        (Value::String(_), Value::Number(_)) => Ok(to_number(lhs)? == to_number(rhs)?),
        (Value::Bool(_), _) => Ok(loose_equal(&Value::Number(to_number(lhs)?), rhs)?),
        (_, Value::Bool(_)) => Ok(loose_equal(lhs, &Value::Number(to_number(rhs)?))?),
        _ => Ok(false),
    }
}

fn to_number(value: &Value) -> Result<f64, VmError> {
    match value {
        Value::Undefined => Ok(f64::NAN),
        Value::Null => Ok(0.0),
        Value::Bool(value) => Ok(if *value { 1.0 } else { 0.0 }),
        Value::Number(value) => Ok(*value),
        Value::String(value) => parse_js_number(value),
        Value::Object(_) => Err(VmError::InvalidState(
            "numeric conversion of objects is not implemented in this PoC".to_owned(),
        )),
    }
}

fn to_number_lossy(value: &Value) -> f64 {
    to_number(value).unwrap_or(f64::NAN)
}

fn parse_js_number(value: &str) -> Result<f64, VmError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(0.0);
    }
    if trimmed == "Infinity" || trimmed == "+Infinity" {
        return Ok(f64::INFINITY);
    }
    if trimmed == "-Infinity" {
        return Ok(f64::NEG_INFINITY);
    }
    trimmed.parse::<f64>().map_err(|_| {
        VmError::InvalidState(format!("cannot convert {value:?} to number in this PoC"))
    })
}

fn to_int32(value: &Value) -> Result<i32, VmError> {
    Ok(to_int32_lossy(value))
}

fn to_int32_lossy(value: &Value) -> i32 {
    let number = to_number_lossy(value);
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let integer = number.trunc().rem_euclid(4294967296.0);
    let unsigned = integer as u32;
    unsigned as i32
}

fn to_uint32(value: &Value) -> Result<u32, VmError> {
    Ok(to_int32(value)? as u32)
}

fn to_js_string(value: &Value) -> String {
    match value {
        Value::Undefined => "undefined".to_owned(),
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => {
            if value.is_nan() {
                "NaN".to_owned()
            } else if *value == f64::INFINITY {
                "Infinity".to_owned()
            } else if *value == f64::NEG_INFINITY {
                "-Infinity".to_owned()
            } else {
                value.to_string()
            }
        }
        Value::String(value) => value.clone(),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

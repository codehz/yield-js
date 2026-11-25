//! Durable state for one Fetch invocation. Transport registrations, sockets
//! and background queues are host state and never enter a snapshot: a restored
//! transaction that still depends on external work is interrupted instead of
//! resumed, so no request is replayed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::body::BodyRecord;
use crate::error::VmError;
use crate::fetch::{FetchEvent, HopRequest, ResponseHead};
use crate::heap::{Heap, Object};
use crate::value::ObjectId;
use crate::vm::VmCore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum FetchStage {
    /// Waiting for the final response head of the current hop.
    AwaitingHead,
    /// Script has the response; body bytes may still arrive.
    Streaming,
    /// Promise settled and body stream finished or errored.
    Done,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum Upload {
    None,
    /// Bytes and how many have been sent.
    Buffered {
        bytes: std::sync::Arc<Vec<u8>>,
        sent: u64,
    },
    /// Stream source with the reader that owns its lock.
    Streaming {
        body: BodyRecord,
        reader: ObjectId,
        finished: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FetchTransaction {
    /// Promise returned to script.
    pub(crate) promise: ObjectId,
    /// Script `Request` that started this fetch; kept for metadata and snapshot edges.
    pub(crate) request: ObjectId,
    pub(crate) signal: Option<ObjectId>,
    pub(crate) stage: FetchStage,
    /// Parameters of the hop currently in flight or about to start.
    pub(crate) hop: HopRequest,
    pub(crate) upload: Upload,
    /// Bytes read from the upload stream but not yet accepted by the adapter.
    pub(crate) upload_pending: Vec<u8>,
    pub(crate) upload_credit: u64,
    pub(crate) pending_upload_credit: u64,
    pub(crate) download_credit: u64,
    pub(crate) redirects: u32,
    pub(crate) urls: Vec<url::Url>,
    pub(crate) preflight: bool,
    pub(crate) buffer: Option<Vec<u8>>,
    pub(crate) upload_read: Option<ObjectId>,
    pub(crate) terminal_queued: bool,
    pub(crate) interruption_queued: bool,
    pub(crate) response: Option<ObjectId>,
    pub(crate) response_stream: Option<ObjectId>,
    /// The transaction has accepted a terminal event and settled its promise.
    pub(crate) settled: bool,
    pub(crate) head: Option<ResponseHead>,
    /// Accepted events that have not yet run as ordinary tasks, in order.
    pub(crate) queued: u64,
}

impl FetchTransaction {
    /// Object edges from durable fetch state.
    pub(crate) fn edges(&self, out: &mut Vec<ObjectId>) {
        out.push(self.promise);
        out.push(self.request);
        out.extend(self.signal);
        out.extend(self.response);
        out.extend(self.response_stream);
        out.extend(self.upload_read);
        if let Upload::Streaming { body, reader, .. } = &self.upload {
            out.push(body.stream);
            out.push(*reader);
        }
    }

    pub(crate) fn is_external(&self) -> bool {
        self.stage != FetchStage::Done && !self.terminal_queued
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct FetchState {
    pub(crate) transactions: BTreeMap<u64, FetchTransaction>,
}

impl FetchState {
    pub(crate) fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.transactions.values().flat_map(|transaction| {
            let mut ids = Vec::new();
            transaction.edges(&mut ids);
            ids
        })
    }

    /// Restore validation: object ids must exist and stages must agree.
    pub(crate) fn validate(&self, heap: &Heap) -> Result<(), VmError> {
        for (id, transaction) in &self.transactions {
            if transaction.settled != matches!(transaction.stage, FetchStage::Done) {
                return Err(VmError::InvalidSnapshot(format!(
                    "fetch {id} has an inconsistent settlement stage"
                )));
            }
            if !matches!(heap.get(transaction.promise), Ok(Object::Promise(_))) {
                return Err(VmError::InvalidSnapshot(format!(
                    "fetch {id} targets a missing promise"
                )));
            }
            if !matches!(heap.get(transaction.request), Ok(Object::Request { .. })) {
                return Err(VmError::InvalidSnapshot(format!(
                    "fetch {id} references a missing request"
                )));
            }
            match (transaction.response, transaction.response_stream) {
                (None, None) => {
                    if transaction.stage == FetchStage::Streaming {
                        return Err(VmError::InvalidSnapshot(format!(
                            "fetch {id} streams without a response"
                        )));
                    }
                }
                (Some(response), stream) => {
                    let valid = matches!(heap.get(response), Ok(Object::Response { data, .. })
                        if data.body.as_ref().map(|body| body.stream) == stream)
                        && stream
                            .is_none_or(|id| matches!(heap.get(id), Ok(Object::ReadableStream(_))));
                    if !valid {
                        return Err(VmError::InvalidSnapshot(format!(
                            "fetch {id} has an invalid response pair"
                        )));
                    }
                }
                (None, Some(_)) => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "fetch {id} has a stream without a response"
                    )));
                }
            }
            if transaction.queued > 0 && transaction.settled {
                return Err(VmError::InvalidSnapshot(format!(
                    "fetch {id} queued events after settlement"
                )));
            }
        }
        Ok(())
    }

    /// Full-mode audit of accepted-but-unprocessed adapter events: counts must
    /// match the transactions, and no stream may have two heads or two EOFs.
    pub(crate) fn validate_queue(
        &self,
        tasks: &[crate::snapshot::OrdinaryTask],
    ) -> Result<(), VmError> {
        use crate::snapshot::OrdinaryTask;

        let mut queued = BTreeMap::<u64, u64>::new();
        let mut heads = std::collections::BTreeSet::new();
        let mut eofs = std::collections::BTreeSet::new();
        for task in tasks {
            let OrdinaryTask::FetchEvent(id, event) = task else {
                continue;
            };
            if !self.transactions.contains_key(id) {
                return Err(VmError::InvalidSnapshot(format!(
                    "queued fetch event for unknown transaction {id}"
                )));
            }
            *queued.entry(*id).or_default() += 1;
            match event {
                FetchEvent::Head(_) if !heads.insert(*id) => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "fetch {id} queued a second response head"
                    )));
                }
                FetchEvent::Eof if !eofs.insert(*id) => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "fetch {id} queued a second EOF"
                    )));
                }
                _ => {}
            }
        }
        for (id, transaction) in &self.transactions {
            if queued.get(id).copied().unwrap_or(0) != transaction.queued {
                return Err(VmError::InvalidSnapshot(format!(
                    "fetch {id} queued event count is inconsistent"
                )));
            }
        }
        Ok(())
    }
}

impl VmCore {
    /// Accepts one adapter event for a live transaction. Runs on the VM thread
    /// but never executes script: ordering and credit are checked here, then
    /// the event is queued as an ordinary task. Events for unknown, cancelled
    /// or older-generation tokens are ignored.
    pub fn deliver_fetch_event(
        &mut self,
        token: crate::binding::OperationToken,
        event: FetchEvent,
    ) -> Result<(), VmError> {
        if token.epoch != self.epoch || !self.fetch_state.transactions.contains_key(&token.id()) {
            return Ok(());
        }
        let id = token.id();
        let transaction = self
            .fetch_state
            .transactions
            .get_mut(&id)
            .expect("transaction checked");
        if transaction.settled || transaction.terminal_queued {
            return Ok(());
        }
        match &event {
            FetchEvent::UploadCredit(amount) => {
                transaction.pending_upload_credit = transaction
                    .pending_upload_credit
                    .checked_add(*amount)
                    .filter(|pending| pending.checked_add(transaction.upload_credit).is_some())
                    .ok_or_else(|| {
                        VmError::HostFault(format!("fetch {id} upload credit overflow"))
                    })?;
            }
            FetchEvent::Informational(_) => return Ok(()),
            FetchEvent::Head(_) => {
                if transaction.head.is_some() || transaction.stage != FetchStage::AwaitingHead {
                    return Err(VmError::HostFault(format!(
                        "fetch {id} received a second response head"
                    )));
                }
                transaction.head = match &event {
                    FetchEvent::Head(head) => Some(head.clone()),
                    _ => unreachable!(),
                };
            }
            FetchEvent::Data(bytes) => {
                if transaction.head.is_none() || transaction.stage == FetchStage::Done {
                    return Err(VmError::HostFault(format!(
                        "fetch {id} received body bytes before the head"
                    )));
                }
                let length = bytes.len() as u64;
                transaction.download_credit = transaction
                    .download_credit
                    .checked_sub(length)
                    .ok_or_else(|| {
                        VmError::HostFault(format!("fetch {id} exceeded download credit"))
                    })?;
            }
            FetchEvent::Eof => {
                if transaction.head.is_none() || transaction.stage == FetchStage::Done {
                    return Err(VmError::HostFault(format!(
                        "fetch {id} received EOF outside an open body"
                    )));
                }
            }
            FetchEvent::Failed(_) => {}
        }
        if matches!(event, FetchEvent::Eof | FetchEvent::Failed(_)) {
            transaction.terminal_queued = true;
        }
        transaction.queued = transaction
            .queued
            .checked_add(1)
            .ok_or_else(|| VmError::HostFault(format!("fetch {id} event count overflow")))?;
        self.ordinary_tasks
            .push_back(crate::snapshot::OrdinaryTask::FetchEvent(id, event));
        Ok(())
    }
}

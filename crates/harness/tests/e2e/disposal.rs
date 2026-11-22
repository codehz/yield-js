//! Binary persistence of an async disposal cursor and validation of its reaction.

use yield_vm::heap::ReactionJob;
use yield_vm::{Completion, JsValue, Object, PromiseState, Snapshot, Value, VmError};

use super::support::Suspended;

#[test]
fn resource_disposal_reaction_round_trips_and_rejects_corruption() {
    let mut run = Suspended::start(
        r#"
import { wait } from "test";
export async function run() {
    const stack = new AsyncDisposableStack();
    let trace = "";
    stack.defer(() => { trace += "A"; throw "A"; });
    stack.defer(() => { trace += "B"; return wait("b"); });
    try { await stack.disposeAsync(); }
    catch (e) {
        return trace === "BA" && e instanceof SuppressedError &&
            e.error === "A" && e.suppressed === "B";
    }
    return false;
}
"#,
        vec![],
    );
    let snapshot = run.snapshot();
    let decoded = Snapshot::from_bytes(&snapshot.to_bytes().unwrap()).unwrap();
    let mut broken = decoded.clone();
    let job = broken
        .objects
        .iter_mut()
        .find_map(|record| {
            let Object::Promise(promise) = &mut record.object else {
                return None;
            };
            promise
                .reactions
                .iter_mut()
                .find(|job| matches!(job, ReactionJob::DisposeNext { .. }))
        })
        .expect("pending disposal reaction");
    let ReactionJob::DisposeNext { state, result } = job else {
        unreachable!()
    };
    *state = *result;
    assert!(matches!(
        run.restore_validated(broken),
        Err(VmError::InvalidSnapshot(_))
    ));

    run.restore_validated(decoded).unwrap();
    run.complete("wait:b", Completion::Rejected(JsValue::string("B")));
    assert_eq!(run.state(), PromiseState::Fulfilled(Value::boolean(true)));
}

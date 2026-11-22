//! One workload and one collection policy per process for independent peak RSS.

use std::time::Instant;

use yield_harness::{TestHost, build, number};
use yield_vm::{Host, Value, Vm};

use crate::workloads::WORKLOADS;

const ROUNDS: u32 = 64;

pub(super) fn run(workload: &str, policy: &str) {
    enum Trigger {
        None,
        Every(u32),
        /// Collect once the allocation delta since the last collection crosses
        /// either threshold: `objects` object allocations or `bytes` of buffer
        /// backing storage, whichever comes first. Zero disables a component.
        Alloc {
            objects: u64,
            bytes: u64,
        },
    }
    let trigger = match policy {
        "none" => Trigger::None,
        "every-1" => Trigger::Every(1),
        "every-8" => Trigger::Every(8),
        other => {
            let rest = other
                .strip_prefix("alloc-")
                .unwrap_or_else(|| panic!("unknown GC policy: {other}"));
            let mut parts = rest.split('-');
            let objects: u64 = parts
                .next()
                .expect("object threshold")
                .parse()
                .expect("integer object threshold");
            let bytes: u64 = parts
                .next()
                .expect("byte threshold")
                .parse()
                .expect("integer byte threshold");
            assert!(parts.next().is_none(), "unknown GC policy: {other}");
            Trigger::Alloc { objects, bytes }
        }
    };
    let react = match workload {
        "react-updates" => Some(
            WORKLOADS
                .iter()
                .find(|workload| workload.name == "react-updates")
                .expect("React updates workload"),
        ),
        "buffer-churn" => None,
        _ => panic!("unknown GC workload: {workload}"),
    };
    let build = match react {
        Some(react) => (react.build)(),
        None => build(include_str!("gc-buffer.js")),
    };
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("start GC workload");
    let mut pauses = Vec::with_capacity(match &trigger {
        Trigger::None => 0,
        Trigger::Every(n) => (ROUNDS / n) as usize,
        Trigger::Alloc { .. } => ROUNDS as usize,
    });
    let mut load_baseline = vm.allocation_totals();

    let start = Instant::now();
    for round in 1..=ROUNDS {
        if let Some(react) = react {
            (react.run)(&mut vm, &mut host);
        } else {
            let marker = f64::from(round);
            let value = vm
                .start(&mut host, "run", vec![number(marker)])
                .expect("allocate and touch buffer");
            vm.run_until_idle(&mut host).expect("drain buffer workload");
            assert_eq!(value, Value::number(marker));
        }
        let due = match &trigger {
            Trigger::None => false,
            Trigger::Every(n) => round % n == 0,
            Trigger::Alloc { objects, bytes } => {
                let (allocated_objects, allocated_bytes) = vm.allocation_totals();
                allocated_objects - load_baseline.0 >= *objects
                    || allocated_bytes - load_baseline.1 >= *bytes
            }
        };
        if due {
            let roots = host.gc_roots();
            let start = Instant::now();
            let removed = vm.collect_garbage(roots).expect("collect at idle boundary");
            let (allocated_objects, allocated_bytes) = vm.allocation_totals();
            pauses.push((
                round,
                start.elapsed(),
                removed,
                allocated_objects - load_baseline.0,
                allocated_bytes - load_baseline.1,
            ));
            load_baseline = vm.allocation_totals();
        }
    }
    let elapsed = start.elapsed();
    assert!(host.errors().is_empty(), "host errors: {:?}", host.errors());
    assert!(host.pending_labels().is_empty(), "unexpected pending work");

    println!("workload,policy,round,metric,value,unit");
    for (round, pause, removed, load_objects, load_bytes) in pauses {
        println!(
            "{workload},{policy},{round},gc_pause,{},ns",
            pause.as_nanos()
        );
        println!("{workload},{policy},{round},gc_removed,{removed},count");
        println!("{workload},{policy},{round},gc_load_objects,{load_objects},count");
        println!("{workload},{policy},{round},gc_load_bytes,{load_bytes},bytes");
    }
    println!(
        "{workload},{policy},{ROUNDS},loop_total,{},ns",
        elapsed.as_nanos()
    );
}

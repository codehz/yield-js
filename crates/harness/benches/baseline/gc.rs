//! One workload and one collection policy per process for independent peak RSS.

use std::time::Instant;

use yield_harness::{TestHost, build, number};
use yield_vm::{Host, Value, Vm};

use crate::workloads::WORKLOADS;

const ROUNDS: u32 = 64;

pub(super) fn run(workload: &str, policy: &str) {
    let interval = match policy {
        "none" => None,
        "every-1" => Some(1),
        "every-8" => Some(8),
        _ => panic!("unknown GC policy: {policy}"),
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
    let mut pauses = Vec::with_capacity(interval.map_or(0, |n| (ROUNDS / n) as usize));

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
        if interval.is_some_and(|n| round % n == 0) {
            let roots = host.gc_roots();
            let start = Instant::now();
            let removed = vm.collect_garbage(roots).expect("collect at idle boundary");
            pauses.push((round, start.elapsed(), removed));
        }
    }
    let elapsed = start.elapsed();
    assert!(host.errors().is_empty(), "host errors: {:?}", host.errors());
    assert!(host.pending_labels().is_empty(), "unexpected pending work");

    println!("workload,policy,round,metric,value,unit");
    for (round, pause, removed) in pauses {
        println!(
            "{workload},{policy},{round},gc_pause,{},ns",
            pause.as_nanos()
        );
        println!("{workload},{policy},{round},gc_removed,{removed},count");
    }
    println!(
        "{workload},{policy},{ROUNDS},loop_total,{},ns",
        elapsed.as_nanos()
    );
}

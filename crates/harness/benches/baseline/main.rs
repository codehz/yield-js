//! Reproducible release baseline. Timing and VM counters run in separate binaries.

mod workloads;

use std::hint::black_box;
use std::time::{Duration, Instant};

use workloads::{WORKLOADS, Workload};
use yield_harness::TestHost;
use yield_vm::{Build, Snapshot, Vm, perf};

const WARMUP_SAMPLES: usize = 2;
const STEADY_ITERATIONS: usize = 5;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut samples = 9;
    let mut compile_only = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bench" => {}
            "--samples" => {
                samples = args.next().expect("sample count").parse().expect("integer");
                assert!(samples > 0, "sample count must be positive");
            }
            "--compile-only" => compile_only = Some(args.next().expect("workload name")),
            _ => panic!("unknown baseline option: {arg}"),
        }
    }
    if let Some(name) = compile_only {
        let workload = WORKLOADS
            .iter()
            .find(|workload| workload.name == name)
            .expect("known workload");
        println!("{}", black_box((workload.build)()).build_id);
        return;
    }

    println!("workload,metric,sample,value,unit");
    for workload in WORKLOADS {
        run(workload, if perf::ENABLED { 1 } else { samples });
    }
}

fn run(workload: &Workload, samples: usize) {
    let build = (workload.build)();
    let artifact = build.to_bytes();
    println!("{},build_id,0,{},text", workload.name, build.build_id);
    if !perf::ENABLED {
        let instructions: usize = build.functions.iter().map(|f| f.code.len()).sum();
        let slots: usize = build
            .functions
            .iter()
            .map(|f| usize::from(f.transient_slots) + usize::from(f.persistent_slots))
            .sum();
        let name = workload.name;
        println!("{name},artifact_size,0,{},bytes", artifact.len());
        println!("{name},static_instructions,0,{instructions},count");
        println!("{name},static_frame_slots,0,{slots},count");
    }
    let warmups = if perf::ENABLED { 0 } else { WARMUP_SAMPLES };
    for i in 0..warmups + samples {
        let mut sample = Sample::default();
        if !perf::ENABLED {
            let compiled = sample.time("compile", workload.build);
            assert_eq!(compiled.build_id, build.build_id, "non-deterministic build");
            let encoded = sample.time("artifact_encode", || black_box(&compiled).to_bytes());
            assert_eq!(encoded, artifact, "non-deterministic artifact");
        }
        let decoded: Build = sample.time("artifact_decode", || {
            Build::from_bytes(black_box(&artifact)).expect("decode build artifact")
        });
        let mut host = TestHost::default();
        let mut vm = if perf::ENABLED {
            sample.runtime("startup", || Vm::new(decoded, &mut host).unwrap())
        } else {
            let (vm, stages) = Vm::new_profiled(decoded, &mut host).expect("initialize VM");
            sample.times.push(("startup_prepare", stages.prepare));
            sample
                .times
                .push(("startup_build_identity", stages.build_identity));
            sample
                .times
                .push(("startup_build_digest", stages.build_digest));
            sample
                .times
                .push(("startup_build_structure", stages.build_structure));
            sample
                .times
                .push(("startup_link_and_setup", stages.link_and_setup));
            sample.times.push(("startup_initialize", stages.initialize));
            vm
        };
        sample.runtime("entry_first", || (workload.run)(&mut vm, &mut host));
        sample.runtime("entry_steady_5", || {
            for _ in 0..STEADY_ITERATIONS {
                (workload.run)(&mut vm, &mut host);
            }
        });
        drop(vm);
        drop(host);

        if let (Some(suspend), Some(resume)) = (workload.suspend, workload.resume) {
            // Fresh VM: the image must not depend on prior benchmark iterations.
            let mut host = TestHost::default();
            let mut vm = Vm::new(build.clone(), &mut host).expect("initialize snapshot source");
            let roots = sample.runtime("suspend", || suspend(&mut vm, &mut host));
            let snapshot = sample.time("snapshot_capture", || {
                vm.snapshot(&roots).expect("capture snapshot")
            });
            let json = sample.time("snapshot_encode", || snapshot.to_json().unwrap());
            if !perf::ENABLED {
                sample.sizes.push(("snapshot_size", json.len()));
            }
            let image = sample.time("snapshot_decode", || Snapshot::from_json(&json).unwrap());
            drop(vm);
            drop(host);
            let mut host = TestHost::default();
            let restore_build = build.clone();
            let mut restored = sample.runtime("restore", || {
                Vm::restore(image, restore_build, &mut host).expect("restore same build")
            });
            // The roots are host-held Values; their ObjectIds survive in the image.
            sample.time("restore_rearm", || {
                restored.start_recovered_operations().unwrap();
            });
            sample.runtime("resume_first", || resume(&mut restored, &mut host, &roots));
        }
        if i >= warmups {
            sample.print(workload.name, i - warmups + 1);
        }
    }
}

#[derive(Default)]
struct Sample {
    times: Vec<(&'static str, Duration)>,
    sizes: Vec<(&'static str, usize)>,
    counters: Vec<(&'static str, perf::Counters)>,
}

impl Sample {
    fn time<T>(&mut self, metric: &'static str, run: impl FnOnce() -> T) -> T {
        if perf::ENABLED {
            return run();
        }
        let start = Instant::now();
        let result = black_box(run());
        self.times.push((metric, start.elapsed()));
        result
    }

    fn runtime<T>(&mut self, metric: &'static str, run: impl FnOnce() -> T) -> T {
        if !perf::ENABLED {
            return self.time(metric, run);
        }
        perf::reset();
        let result = run();
        self.counters.push((metric, perf::read()));
        result
    }

    fn print(self, name: &str, sample: usize) {
        for (metric, elapsed) in self.times {
            println!("{name},{metric},{sample},{},ns", elapsed.as_nanos());
        }
        for (metric, bytes) in self.sizes {
            println!("{name},{metric},{sample},{bytes},bytes");
        }
        for (phase, c) in self.counters {
            for (metric, value) in [
                ("dispatches", c.dispatches),
                ("frame_allocations", c.frame_allocations),
                ("frame_slots", c.frame_slots),
                ("property_lookups", c.property_lookups),
                ("binding_lookups", c.binding_lookups),
                ("property_slow_paths", c.property_slow_paths),
                ("saved_values", c.saved_values),
                ("suspensions", c.suspensions),
            ] {
                println!("{name},{phase}.{metric},{sample},{value},count");
            }
        }
    }
}

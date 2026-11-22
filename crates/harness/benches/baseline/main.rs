//! Reproducible release baseline. Timing and VM counters run in separate binaries.

mod gc;
mod workloads;

use std::hint::black_box;
use std::time::{Duration, Instant};

use workloads::{REACT_REUSE, REMATERIALIZATION, SNAPSHOT_BUFFER, WORKLOADS, Workload};
use yield_harness::TestHost;
use yield_vm::{Build, Snapshot, VerifiedBuild, Vm, perf};

const WARMUP_SAMPLES: usize = 2;
const STEADY_ITERATIONS: usize = 5;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut samples = 9;
    let mut compile_only = None;
    let mut reuse = false;
    let mut snapshot_codecs = false;
    let mut capture_profile = false;
    let mut restore_profile = false;
    let mut rematerialization = false;
    let mut stack_traces = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bench" => {}
            "--samples" => {
                samples = args.next().expect("sample count").parse().expect("integer");
                assert!(samples > 0, "sample count must be positive");
            }
            "--compile-only" => compile_only = Some(args.next().expect("workload name")),
            "--verified-reuse" if !perf::ENABLED => reuse = true,
            "--verified-reuse" => panic!("reuse timings require a non-diagnostic build"),
            "--snapshot-codecs" if !perf::ENABLED => snapshot_codecs = true,
            "--snapshot-codecs" => panic!("snapshot timings require a non-diagnostic build"),
            "--capture-profile" if !perf::ENABLED => capture_profile = true,
            "--capture-profile" => panic!("capture timings require a non-diagnostic build"),
            "--restore-profile" if !perf::ENABLED => restore_profile = true,
            "--restore-profile" => panic!("restore timings require a non-diagnostic build"),
            "--gc" if !perf::ENABLED => {
                let workload = args.next().expect("GC workload");
                let policy = args.next().expect("GC policy");
                gc::run(&workload, &policy);
                return;
            }
            "--gc" => panic!("GC timings require a non-diagnostic build"),
            "--rematerialization" => rematerialization = true,
            "--stack-traces" if !perf::ENABLED => stack_traces = true,
            "--stack-traces" => panic!("stack trace timings require a non-diagnostic build"),
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
    if stack_traces {
        run_stack_traces(samples);
        return;
    }
    if capture_profile {
        for workload in [&REACT_REUSE, &SNAPSHOT_BUFFER]
            .into_iter()
            .chain(WORKLOADS)
        {
            if workload.suspend.is_some() {
                run_capture_profile(workload, samples);
            }
        }
        return;
    }
    if restore_profile {
        for workload in [&REACT_REUSE].into_iter().chain(WORKLOADS) {
            if workload.suspend.is_some() {
                run_restore_profile(workload, samples);
            }
        }
        return;
    }
    if snapshot_codecs {
        for workload in [&REACT_REUSE].into_iter().chain(WORKLOADS) {
            if workload.suspend.is_some() {
                run_snapshot_codecs(workload, samples);
            }
        }
        return;
    }
    if reuse {
        let await_success = WORKLOADS
            .iter()
            .find(|w| w.name == "await-success")
            .unwrap();
        for workload in [&REACT_REUSE, await_success] {
            run_reuse(workload, samples);
        }
        return;
    }
    if rematerialization {
        for workload in REMATERIALIZATION {
            run(workload, if perf::ENABLED { 1 } else { samples });
        }
        return;
    }
    for workload in WORKLOADS {
        run(workload, if perf::ENABLED { 1 } else { samples });
    }
}

fn run_reuse(workload: &Workload, samples: usize) {
    const VMS: usize = 8;
    let suspend = workload
        .suspend
        .expect("reuse workload captures a snapshot");
    let resume = workload
        .resume
        .expect("reuse workload checks restored state");
    let artifact = (workload.build)().to_bytes();
    let decode = || Build::from_bytes(&artifact).expect("decode build artifact");
    let mut source_host = TestHost::default();
    let mut source = Vm::new(decode(), &mut source_host).expect("start snapshot source");
    let roots = suspend(&mut source, &mut source_host);
    let snapshot = source.snapshot(&roots).expect("capture source");
    drop(source);
    drop(source_host);

    let name = workload.name;
    println!("{name},build_id,0,{},text", snapshot.build_id);
    println!("{name},artifact_size,0,{},bytes", artifact.len());
    println!(
        "{name},snapshot_size,0,{},bytes",
        snapshot.to_json().unwrap().len()
    );
    for i in 0..WARMUP_SAMPLES + samples {
        let mut sample = Sample::default();
        let build = decode();
        let verified = sample.time("verify_once", || VerifiedBuild::new(build).unwrap());
        let mut totals = [Duration::ZERO; 4];
        let mut measure = |slot: usize, start: Instant, vm: Vm| {
            totals[slot] += start.elapsed();
            black_box(vm)
        };
        for j in 0..VMS {
            let shared_first = (i + j) % 2 == 0;
            for shared in [shared_first, !shared_first] {
                let mut host = TestHost::default();
                let mut vm = if shared {
                    let start = Instant::now();
                    measure(
                        1,
                        start,
                        Vm::new_verified(verified.clone(), &mut host).unwrap(),
                    )
                } else {
                    let build = decode();
                    let start = Instant::now();
                    measure(0, start, Vm::new(build, &mut host).unwrap())
                };
                (workload.run)(&mut vm, &mut host);

                let mut host = TestHost::default();
                let image = snapshot.clone();
                let mut vm = if shared {
                    let start = Instant::now();
                    measure(
                        3,
                        start,
                        Vm::restore_verified(image, verified.clone(), &mut host).unwrap(),
                    )
                } else {
                    let build = decode();
                    let start = Instant::now();
                    measure(2, start, Vm::restore(image, build, &mut host).unwrap())
                };
                vm.start_recovered_operations().unwrap();
                resume(&mut vm, &mut host, &roots);
            }
        }
        sample.times.extend(
            [
                "startup_8",
                "startup_verified_8",
                "restore_8",
                "restore_verified_8",
            ]
            .into_iter()
            .zip(totals),
        );
        if i >= WARMUP_SAMPLES {
            sample.print(name, i - WARMUP_SAMPLES);
        }
    }
}

fn run_restore_profile(workload: &Workload, samples: usize) {
    let program = VerifiedBuild::new((workload.build)()).expect("verify build");
    let mut host = TestHost::default();
    let mut source = Vm::new_verified(program.clone(), &mut host).expect("start snapshot source");
    let roots = workload.suspend.unwrap()(&mut source, &mut host);
    let snapshot = source.snapshot(&roots).expect("capture source");
    drop(source);
    drop(host);
    let bytes = snapshot.to_bytes().unwrap();

    let name = workload.name;
    println!("{name},build_id,0,{},text", snapshot.build_id);
    println!("{name},snapshot_objects,0,{},count", snapshot.objects.len());
    for i in 0..WARMUP_SAMPLES + samples {
        let mut sample = Sample::default();
        let profiled_first = i % 2 == 1;
        for profiled in [profiled_first, !profiled_first] {
            let image = Snapshot::from_bytes(&bytes).expect("decode binary snapshot");
            let mut host = TestHost::default();
            let build = program.clone();
            let mut vm = if profiled {
                let (vm, profile) = Vm::restore_verified_profiled(image, build, &mut host)
                    .expect("restore and profile");
                sample.times.extend([
                    ("profiled_restore", profile.total),
                    ("compatibility", profile.compatibility),
                    ("heap_insert", profile.heap_insert),
                    ("heap_side_tables", profile.heap_side_tables),
                    ("native_validation", profile.native_validation),
                    ("state_load", profile.state_load),
                    ("vm_assembly", profile.vm_assembly),
                    ("graph_validation", profile.graph_validation),
                    ("finalize", profile.finalize),
                ]);
                vm
            } else {
                sample.time("restore", || {
                    Vm::restore_verified(image, build, &mut host).expect("restore")
                })
            };
            vm.start_recovered_operations().unwrap();
            workload.resume.unwrap()(&mut vm, &mut host, &roots);
        }
        if i >= WARMUP_SAMPLES {
            sample.print(name, i - WARMUP_SAMPLES);
        }
    }
}

fn run_capture_profile(workload: &Workload, samples: usize) {
    let program = VerifiedBuild::new((workload.build)()).expect("verify build");
    let mut source_host = TestHost::default();
    let mut source = Vm::new_verified(program.clone(), &mut source_host).expect("start source");
    let roots = workload.suspend.unwrap()(&mut source, &mut source_host);
    let name = workload.name;
    println!("{name},build_id,0,{},text", program.build_id);

    for i in 0..WARMUP_SAMPLES + samples {
        let mut sample = Sample::default();
        let profiled_first = i % 2 == 1;
        let mut reference_bytes: Option<Vec<u8>> = None;
        for profiled in [profiled_first, !profiled_first] {
            let snapshot = if profiled {
                let (snapshot, profile) =
                    source.snapshot_profiled(&roots).expect("profile capture");
                assert_eq!(
                    profile.total,
                    profile.trace + profile.records + profile.host_records
                );
                sample.times.extend([
                    ("profiled_capture", profile.total),
                    ("capture_trace", profile.trace),
                    ("capture_records", profile.records),
                    ("capture_host_records", profile.host_records),
                ]);
                snapshot
            } else {
                sample.time("capture", || source.snapshot(&roots).expect("capture"))
            };
            let bytes = if profiled {
                snapshot.to_bytes().expect("encode profiled snapshot")
            } else {
                sample.time("binary_encode", || {
                    snapshot.to_bytes().expect("encode snapshot")
                })
            };
            if i == 0 && !profiled {
                println!("{name},snapshot_objects,0,{},count", snapshot.objects.len());
                println!("{name},binary_size,0,{},bytes", bytes.len());
            }
            drop(snapshot);
            let image = Snapshot::from_bytes(&bytes).expect("decode binary snapshot");
            if let Some(reference) = reference_bytes.take() {
                assert!(bytes == reference, "capture bytes differ for {name}");
            } else {
                reference_bytes = Some(bytes);
            }
            let mut host = TestHost::default();
            let mut vm = Vm::restore_verified(image, program.clone(), &mut host).expect("restore");
            vm.start_recovered_operations().expect("rearm operations");
            workload.resume.unwrap()(&mut vm, &mut host, &roots);
        }
        if i >= WARMUP_SAMPLES {
            sample.print(name, i - WARMUP_SAMPLES);
        }
    }
}

fn run_snapshot_codecs(workload: &Workload, samples: usize) {
    let program = VerifiedBuild::new((workload.build)()).expect("verify build");
    let mut host = TestHost::default();
    let mut source = Vm::new_verified(program.clone(), &mut host).expect("start snapshot source");
    let roots = workload.suspend.unwrap()(&mut source, &mut host);
    let snapshot = source.snapshot(&roots).expect("capture source");
    drop(source);
    drop(host);

    let name = workload.name;
    println!("{name},build_id,0,{},text", snapshot.build_id);
    println!(
        "{name},json_size,0,{},bytes",
        snapshot.to_json().unwrap().len()
    );
    println!(
        "{name},binary_size,0,{},bytes",
        snapshot.to_bytes().unwrap().len()
    );
    for i in 0..WARMUP_SAMPLES + samples {
        let mut sample = Sample::default();
        let binary_first = i % 2 == 1;
        for binary in [binary_first, !binary_first] {
            let (image, metric) = if binary {
                let bytes = sample.time("binary_encode", || snapshot.to_bytes().unwrap());
                let image = sample.time("binary_decode", || Snapshot::from_bytes(&bytes).unwrap());
                (image, "binary_restore")
            } else {
                let json = sample.time("json_encode", || snapshot.to_json().unwrap());
                let image = sample.time("json_decode", || Snapshot::from_json(&json).unwrap());
                (image, "json_restore")
            };
            let mut host = TestHost::default();
            let program = program.clone();
            let mut vm = sample.time(metric, || {
                Vm::restore_verified(image, program, &mut host).unwrap()
            });
            vm.start_recovered_operations().unwrap();
            workload.resume.unwrap()(&mut vm, &mut host, &roots);
        }
        if i >= WARMUP_SAMPLES {
            sample.print(name, i - WARMUP_SAMPLES);
        }
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

fn run_stack_traces(samples: usize) {
    use yield_compiler::{CompileOptions, MemoryResolver, compile};
    use yield_vm::{ValueRef, VerifiedBuild};

    const ERRORS: usize = 4096;
    let entry = format!(
        "import {{ relay }} from './vendor.js';
         function make() {{ return new Error('benchmark'); }}
         export function run() {{
             let last;
             for (let n = 0; n < {ERRORS}; n++) last = relay(make);
             return last;
         }}"
    );
    let resolver = MemoryResolver::new([
        ("main.js", entry.as_str()),
        (
            "vendor.js",
            "export function relay(callback) { return callback(); }",
        ),
    ])
    .unwrap();
    let mut builds = Vec::new();
    for (name, stack_traces, hide_vendor) in [
        ("stacks-off", false, false),
        ("stacks-on", true, false),
        ("stacks-hide-vendor", true, true),
    ] {
        let mut options = CompileOptions::from_dts(include_str!("../../test.d.ts")).unwrap();
        options.stack_traces = stack_traces;
        let mut error_options = options.clone();
        if hide_vendor {
            options.framework_modules.extend([
                "react.mjs".into(),
                "react-reconciler-0.31.0.mjs".into(),
                "renderer.mjs".into(),
            ]);
            error_options.framework_modules.insert("vendor.js".into());
        }
        let bytes = workloads::build_react_with_options(&options)
            .to_bytes()
            .len();
        println!("{name},react_artifact_size,0,{bytes},bytes");
        let build = compile("main.js", None, &resolver, &error_options).unwrap();
        builds.push((name, VerifiedBuild::new(build).unwrap()));
    }
    for i in 0..WARMUP_SAMPLES + samples {
        // Rotate order and use fresh heaps to avoid systematic warmup/sweep bias.
        for (name, build) in builds
            .iter()
            .cycle()
            .skip(i % builds.len())
            .take(builds.len())
        {
            let mut host = TestHost::default();
            let mut vm = Vm::new_verified(build.clone(), &mut host).unwrap();
            let start = Instant::now();
            let result = black_box(vm.start(&mut host, "run", vec![]).unwrap());
            let elapsed = start.elapsed();
            assert!(matches!(result.view(), ValueRef::Object(_)));
            if i >= WARMUP_SAMPLES {
                let sample = i - WARMUP_SAMPLES + 1;
                let per_error = elapsed.as_nanos() as f64 / ERRORS as f64;
                println!("{name},error_loop_per_error,{sample},{per_error:.2},ns");
            }
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

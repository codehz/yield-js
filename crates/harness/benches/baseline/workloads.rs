use yield_compiler::{CompileOptions, MemoryResolver, compile};
use yield_harness::{TestHost, build, number, string};
use yield_vm::{Build, Completion, JsValue, PromiseState, Value, Vm};

type Suspend = fn(&mut Vm, &mut TestHost) -> Vec<Value>;
type Resume = fn(&mut Vm, &mut TestHost, &[Value]);

pub(super) struct Workload {
    pub(super) name: &'static str,
    pub(super) build: fn() -> Build,
    pub(super) run: fn(&mut Vm, &mut TestHost),
    /// Stops at a verified suspension and returns roots for the timed capture.
    pub(super) suspend: Option<fn(&mut Vm, &mut TestHost) -> Vec<Value>>,
    /// Continues from original or restored roots. The restore path must rearm
    /// recovered operations before invoking this callback.
    pub(super) resume: Option<fn(&mut Vm, &mut TestHost, &[Value])>,
}

impl Workload {
    const fn new(
        name: &'static str,
        build: fn() -> Build,
        run: fn(&mut Vm, &mut TestHost),
    ) -> Self {
        Self {
            name,
            build,
            run,
            suspend: None,
            resume: None,
        }
    }

    const fn with_snapshot(mut self, suspend: Suspend, resume: Resume) -> Self {
        self.suspend = Some(suspend);
        self.resume = Some(resume);
        self
    }
}

pub(super) const WORKLOADS: &[Workload] = &[
    Workload::new("react-first-commit", build_react, react_first_commit),
    Workload::new("react-updates", build_react, react_updates),
    Workload::new(
        "numeric-branch",
        || build(include_str!("numeric.js")),
        numeric_branch,
    ),
    Workload::new(
        "builtin-math",
        || build(include_str!("builtin.js")),
        |vm, host| {
            assert_number(vm, host, "run", vec![], 6_294_556.0);
        },
    ),
    Workload::new(
        "short-call",
        || build(include_str!("short-call.js")),
        |vm, host| {
            assert_number(vm, host, "run", vec![], 526_848.0);
        },
    ),
    Workload::new(
        "closure",
        || build(include_str!("closure.js")),
        |vm, host| {
            assert_number(vm, host, "run", vec![], 1_536.0);
        },
    ),
    Workload::new(
        "static-property",
        || build(include_str!("static-property.js")),
        |vm, host| {
            assert_number(vm, host, "run", vec![], 3_072.0);
        },
    ),
    Workload::new("await-success", build_await, |vm, host| {
        let roots = suspend_await_success(vm, host);
        resume_await_success(vm, host, &roots);
    })
    .with_snapshot(suspend_await_success, resume_await_success),
    Workload::new("await-reject-finally", build_await, |vm, host| {
        let roots = suspend_await_reject(vm, host);
        resume_await_reject(vm, host, &roots);
    })
    .with_snapshot(suspend_await_reject, resume_await_reject),
    Workload::new("sync-generator", build_generators, |vm, host| {
        let roots = suspend_sync_generator(vm, host);
        resume_generator(vm, host, &roots);
    })
    .with_snapshot(suspend_sync_generator, resume_generator),
    Workload::new("async-generator", build_generators, |vm, host| {
        let roots = suspend_async_generator(vm, host);
        resume_generator(vm, host, &roots);
    })
    .with_snapshot(suspend_async_generator, resume_generator),
];

#[track_caller]
fn start(vm: &mut Vm, host: &mut TestHost, entry: &str, args: Vec<JsValue>) -> Value {
    vm.start(host, entry, args)
        .unwrap_or_else(|error| panic!("{entry}: {error}"))
}

#[track_caller]
fn assert_number(vm: &mut Vm, host: &mut TestHost, entry: &str, args: Vec<JsValue>, expected: f64) {
    assert_eq!(
        start(vm, host, entry, args),
        Value::number(expected),
        "{entry}"
    );
}

fn numeric_branch(vm: &mut Vm, host: &mut TestHost) {
    assert_number(vm, host, "run", vec![number(3.0)], 259_584.0);
    // The same arithmetic sites must also coerce a non-Number operand.
    assert_number(vm, host, "run", vec![string("5")], 258_560.0);
}

fn build_react() -> Build {
    let preload = concat!(
        include_str!("react-preload.mjs"),
        "\nexport const window = undefined; export const process = undefined;\n",
    );
    let resolver = MemoryResolver::new([
        ("main.js", include_str!("react.js")),
        ("preload.js", preload),
        (
            "react.mjs",
            include_str!("../../tests/e2e/vendor/react-19.2.0-esm-sh.mjs"),
        ),
        (
            "react-reconciler-0.31.0.mjs",
            include_str!("../../tests/e2e/vendor/react-reconciler-0.31.0.mjs"),
        ),
        (
            "renderer.mjs",
            include_str!("../../tests/e2e/vendor/react-renderer.mjs"),
        ),
    ])
    .expect("valid React fixture paths");
    let options =
        CompileOptions::from_dts(include_str!("../../test.d.ts")).expect("test declarations parse");
    compile("main.js", Some("preload.js"), &resolver, &options).expect("React fixture compiles")
}

fn react_commit(vm: &mut Vm, host: &mut TestHost) {
    vm.run_until_idle(host).expect("drain React microtasks");
    host.advance(vm, 10).expect("deliver React scheduler work");
    assert!(
        host.errors().is_empty(),
        "React scheduler errors: {:?}",
        host.errors()
    );
}

fn react_first_commit(vm: &mut Vm, host: &mut TestHost) {
    start(vm, host, "first", vec![]);
    react_commit(vm, host);
    assert_number(vm, host, "checkFirst", vec![], 1.0);
}

fn react_updates(vm: &mut Vm, host: &mut TestHost) {
    // The first run on a VM mounts the root; warm runs update that same root.
    if start(vm, host, "hasUpdateRoot", vec![]) == Value::number(0.0) {
        start(vm, host, "mountUpdates", vec![]);
        react_commit(vm, host);
        assert_number(vm, host, "checkUpdateMount", vec![], 1.0);
    }
    // Drain after each update so the scheduler cannot batch away intermediate commits.
    for index in 0..8 {
        start(vm, host, "update", vec![number(f64::from(index))]);
        react_commit(vm, host);
        assert_number(vm, host, "checkUpdate", vec![number(f64::from(index))], 1.0);
    }
}

const AWAIT_LABEL: &str = "wait:baseline-await";
const GENERATOR_LABEL: &str = "wait:baseline-generator";

fn suspend_pending(vm: &mut Vm, host: &mut TestHost, entry: &str, label: &str) -> Vec<Value> {
    let result = start(vm, host, entry, vec![]);
    vm.run_until_idle(host).expect("drain to host wait");
    assert_eq!(host.pending_labels(), [label]);
    assert!(matches!(
        vm.promise_state(&result).expect("read result promise"),
        PromiseState::Pending
    ));
    vec![result]
}

fn finish_pending(
    vm: &mut Vm,
    host: &mut TestHost,
    roots: &[Value],
    label: &str,
    completion: Completion,
    expected: f64,
) {
    let [result] = roots else {
        panic!("expected one result promise root")
    };
    host.complete(vm, label, completion)
        .expect("deliver host completion");
    vm.run_until_idle(host).expect("drain continuation");
    match vm.promise_state(result).expect("read result promise") {
        PromiseState::Fulfilled(value) => assert_eq!(value, Value::number(expected), "{label}"),
        state => panic!("{label}: expected fulfillment, got {state:?}"),
    }
    assert!(
        host.pending_labels().is_empty(),
        "unexpected pending host work"
    );
    assert!(host.errors().is_empty(), "host errors: {:?}", host.errors());
}

fn build_await() -> Build {
    build(include_str!("await.js"))
}

fn suspend_await_success(vm: &mut Vm, host: &mut TestHost) -> Vec<Value> {
    suspend_pending(vm, host, "success", AWAIT_LABEL)
}

fn resume_await_success(vm: &mut Vm, host: &mut TestHost, roots: &[Value]) {
    finish_pending(
        vm,
        host,
        roots,
        AWAIT_LABEL,
        Completion::Fulfilled(number(7.0)),
        2_023.0,
    );
}

fn suspend_await_reject(vm: &mut Vm, host: &mut TestHost) -> Vec<Value> {
    suspend_pending(vm, host, "rejectInFinally", AWAIT_LABEL)
}

fn resume_await_reject(vm: &mut Vm, host: &mut TestHost, roots: &[Value]) {
    finish_pending(
        vm,
        host,
        roots,
        AWAIT_LABEL,
        Completion::Rejected(string("baseline rejection")),
        12.0,
    );
}

fn build_generators() -> Build {
    build(include_str!("generators.js"))
}

fn suspend_sync_generator(vm: &mut Vm, host: &mut TestHost) -> Vec<Value> {
    suspend_pending(vm, host, "sync", GENERATOR_LABEL)
}

fn suspend_async_generator(vm: &mut Vm, host: &mut TestHost) -> Vec<Value> {
    suspend_pending(vm, host, "asyncGenerator", GENERATOR_LABEL)
}

fn resume_generator(vm: &mut Vm, host: &mut TestHost, roots: &[Value]) {
    finish_pending(
        vm,
        host,
        roots,
        GENERATOR_LABEL,
        Completion::Fulfilled(number(20.0)),
        1.0,
    );
}

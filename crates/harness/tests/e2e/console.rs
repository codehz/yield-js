use std::time::Duration;

use yield_compiler::Compiler;
use yield_harness::{TestHost, build};
use yield_vm::console::{
    ConsoleArg, ConsoleFragment, ConsoleKey, ConsoleLevel, ConsolePayload, ConsolePreviewMode,
    ConsoleProperty, ConsoleValue,
};
use yield_vm::{
    ConsoleEvent, RestoreOptions, Snapshot, SnapshotValidation, Value, Vm, VmError, VmOptions,
};

fn args(event: &ConsoleEvent) -> &[ConsoleArg] {
    let ConsolePayload::Args(args) = &event.payload else {
        panic!("{event:?}")
    };
    args
}

fn message(event: &ConsoleEvent) -> String {
    let ConsoleArg::Value(preview) = &args(event)[0] else {
        panic!("{event:?}")
    };
    let ConsoleValue::String(text) = &preview.value else {
        panic!("{event:?}")
    };
    text.to_utf8().unwrap()
}

#[test]
fn console_formatter_keeps_fragments_and_never_invokes_inspection_hooks() {
    let host = TestHost::new();
    let mut vm = Vm::new(
        build(
            r#"
        export function run() {
            let hits = 0;
            const cyclic = { get touch() { hits++; return 7; } };
            cyclic.self = cyclic;
            const proxy = new Proxy({}, {
                ownKeys() { hits++; return []; }, get() { hits++; },
                getOwnPropertyDescriptor() { hits++; }
            });
            globalThis.String = () => { throw 99; };
            globalThis.parseInt = globalThis.String;
            Number.parseFloat = globalThis.String;
            console.log('%s %i %f %O', Symbol('s'), '12px', Symbol(), cyclic);
            console.log('%q %c%o', 'color: red', proxy, '\ud800');
            const marker = {};
            try {
                console.log('%s %s', { toString() { console.info('nested'); return 'first'; } },
                    { toString() { throw marker; } });
            } catch (error) {
                if (error !== marker) throw error;
            }
            const sparse = new Array(10000000);
            sparse[9999999] = 8;
            console.log(sparse, new Array(120).fill(1));
            return hits;
        }
    "#,
        ),
        host.host(),
        Default::default(),
    )
    .unwrap();
    assert_eq!(vm.start("run", vec![]).unwrap(), Value::number(0.0));
    let events = host.take_console_events();
    assert_eq!(events.len(), 4);
    let [ConsoleArg::Formatted(fragments)] = args(&events[0]) else {
        panic!("{events:?}")
    };
    let [
        ConsoleFragment::Text(text),
        ConsoleFragment::Value(integer),
        ConsoleFragment::Text(_),
        ConsoleFragment::Value(float),
        ConsoleFragment::Text(_),
        ConsoleFragment::Value(object),
    ] = &fragments[..]
    else {
        panic!("{fragments:?}")
    };
    assert_eq!(text, "Symbol(s) ");
    assert_eq!(integer.value, ConsoleValue::Number(12.0));
    assert!(matches!(float.value, ConsoleValue::Number(n) if n.is_nan()));
    assert_eq!(object.mode, ConsolePreviewMode::Generic);
    let ConsoleValue::Object(object) = &object.value else {
        panic!("{object:?}")
    };
    assert!(matches!(&object.properties[..], [
        (ConsoleKey::String(touch), ConsoleProperty::Accessor),
        (ConsoleKey::String(this), ConsoleProperty::Data(ConsoleValue::Object(cycle))),
    ] if touch == "touch" && this == "self" && cycle.circular));
    let [ConsoleArg::Formatted(fragments), ConsoleArg::Value(extra)] = args(&events[1]) else {
        panic!("{events:?}")
    };
    assert!(
        matches!(&fragments[..], [ConsoleFragment::Text(text), ConsoleFragment::Style(style), ConsoleFragment::Value(proxy)]
        if text == "%q " && style == "color: red"
            && matches!(&proxy.value, ConsoleValue::Object(proxy) if proxy.kind == "Proxy" && proxy.properties.is_empty()))
    );
    assert!(matches!(&extra.value, ConsoleValue::String(s) if s.units().eq([0xd800])));
    assert_eq!(events[2].level, ConsoleLevel::Info);
    assert_eq!(message(&events[2]), "nested");
    let [ConsoleArg::Value(sparse), ConsoleArg::Value(dense)] = args(&events[3]) else {
        panic!("{events:?}")
    };
    assert!(
        matches!(&sparse.value, ConsoleValue::Object(o) if o.length == Some(10_000_000)
        && !o.truncated && o.properties == [(ConsoleKey::String("9999999".into()), ConsoleProperty::Data(ConsoleValue::Number(8.0)))])
    );
    assert!(
        matches!(&dense.value, ConsoleValue::Object(o) if o.length == Some(120) && o.truncated && o.properties.len() == 100)
    );
}

#[test]
fn console_table_dir_groups_clear_and_trace_have_structured_payloads() {
    let source = r#"
        function helper() { console.trace('frame %i', '3px'); }
        export function run() {
            console.groupCollapsed('outer %d', 4);
            console.group();
            const data = [{ b: 2, a: undefined, ignored: 9 }, { a: 3 }, 7];
            console.table(data, new Set(['a', 'Value', 'b']));
            console.table(data);
            console.dir(data, { depth: 2 });
            helper();
            console.clear();
            console.dirxml({ value: 1 });
        }
    "#;
    for stack_traces in [true, false] {
        let host = TestHost::new();
        let options = Compiler {
            stack_traces,
            ..Default::default()
        };
        let build = super::support::compile_main(source, &options).unwrap();
        let mut vm = Vm::new(build, host.host(), Default::default()).unwrap();
        vm.start("run", vec![]).unwrap();
        let events = host.take_console_events();
        assert_eq!(events.len(), 8);
        assert_eq!(events[0].level, ConsoleLevel::GroupCollapsed);
        assert!(events[0].groups.is_empty());
        assert_eq!(events[1].groups.len(), 1);
        assert!(events[1].groups[0].collapsed);
        assert_eq!(message(&events[1]), "group");
        assert_eq!(events[2].groups.len(), 2);
        assert!(!events[2].groups[1].collapsed);
        assert_eq!(events[2].level, ConsoleLevel::Log);
        let ConsolePayload::Table(table) = &events[2].payload else {
            panic!("{events:?}")
        };
        assert_eq!(table.columns, ["a", "Value", "b"]);
        assert!(!table.truncated);
        assert_eq!(table.rows[0].index, "0");
        assert_eq!(
            table.rows[0].cells,
            [
                Some(ConsoleValue::Undefined),
                None,
                Some(ConsoleValue::Number(2.0))
            ]
        );
        assert_eq!(
            table.rows[1].cells,
            [Some(ConsoleValue::Number(3.0)), None, None]
        );
        assert_eq!(
            table.rows[2].cells,
            [None, Some(ConsoleValue::Number(7.0)), None]
        );
        assert!(
            matches!(&events[3].payload, ConsolePayload::Table(table) if table.columns == ["b", "a", "ignored", "Value"])
        );
        assert!(
            matches!(&events[4].payload, ConsolePayload::Dir { value, options: Some(options) }
            if value.mode == ConsolePreviewMode::Generic && matches!(&options.value, ConsoleValue::Object(options)
                if options.properties == [(ConsoleKey::String("depth".into()), ConsoleProperty::Data(ConsoleValue::Number(2.0)))]))
        );
        let ConsolePayload::Trace {
            args: label,
            frames,
        } = &events[5].payload
        else {
            panic!("{events:?}")
        };
        assert!(matches!(&label[..], [ConsoleArg::Formatted(_)]));
        if stack_traces {
            assert!(
                matches!(&frames[..], [helper, run] if helper.name == "helper" && run.name == "run"
                && helper.module == "main.js" && helper.line == 2 && helper.column > 0)
            );
        } else {
            assert!(frames.is_empty());
        }
        assert_eq!(events[6].payload, ConsolePayload::Clear);
        assert!(events[6].groups.is_empty() && events[7].groups.is_empty());
        assert_eq!(events[7].level, ConsoleLevel::Dirxml);
        assert!(
            matches!(args(&events[7]), [ConsoleArg::Value(p)] if p.mode == ConsolePreviewMode::Optimal)
        );
    }
}

#[test]
fn console_missing_sink_is_lazy_and_preserves_earlier_effects() {
    let build = build(
        r#"
        export function begin() { console.group('saved'); }
        export function run() {
            if (console.assert(true) !== undefined || console.log() !== undefined ||
                console.time('t') !== undefined || console.time('t') !== undefined) throw 'silent';
            const attempted = [() => console.log(1), () => console.group('not pushed'),
                () => console.count('x'), () => console.timeEnd('t'), () => console.clear()];
            for (const attempt of attempted) {
                let caught;
                try { attempt(); } catch (e) { caught = e; }
                if (!(caught instanceof DOMException) || caught.name !== 'NotSupportedError') throw caught;
            }
            const marker = {};
            try { console.log('%s', { toString() { throw marker; } }); }
            catch (e) { return e === marker; }
            return false;
        }
        export function finish() { console.count('x'); console.timeLog('t'); }
    "#,
    );
    let host = TestHost::new();
    let mut vm = Vm::new(build.clone(), host.host(), Default::default()).unwrap();
    vm.start("begin", vec![]).unwrap();
    let missing = TestHost::without_console_service();
    let mut vm = Vm::restore(
        vm.snapshot(&[]).unwrap(),
        build.clone(),
        missing.host(),
        Default::default(),
    )
    .unwrap();
    assert_eq!(vm.start("run", vec![]).unwrap(), Value::boolean(true));
    host.take_console_events();
    let mut vm = Vm::restore(
        vm.snapshot(&[]).unwrap(),
        build,
        host.host(),
        Default::default(),
    )
    .unwrap();
    assert!(host.take_console_events().is_empty());
    vm.start("finish", vec![]).unwrap();
    let events = host.take_console_events();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.groups.is_empty()));
    assert_eq!(message(&events[0]), "x: 2");
    assert_eq!(events[1].level, ConsoleLevel::Warn);
    assert_eq!(message(&events[1]), "No timer for label: t");
}

#[test]
fn console_snapshot_restores_state_without_replaying_events() {
    let build = build(
        r#"
        export function begin() {
            console.groupCollapsed('parent'); console.count('x'); console.time('t');
            // Restore must not require mutable properties to be unchanged.
            delete console[Symbol.toStringTag];
            console.info = 42;
        }
        export function run() {
            console.count('x'); console.timeLog('t', 7); console.timeEnd('t'); console.groupEnd();
            return console.info;
        }
    "#,
    );
    let old_host = TestHost::new();
    let options = VmOptions {
        test_clock: Some(old_host.clock().clone()),
        ..Default::default()
    };
    let mut vm = Vm::new(build.clone(), old_host.host(), options).unwrap();
    vm.start("begin", vec![]).unwrap();
    old_host
        .clock()
        .advance_monotonic(Duration::from_micros(1250));
    let snapshot = vm.snapshot(&[]).unwrap();
    assert!(snapshot.operations.is_empty());
    assert_eq!(old_host.take_console_events().len(), 2);
    let binary = snapshot.to_bytes().unwrap();
    let json = snapshot.to_json().unwrap();
    let missing = TestHost::without_console_service();
    Vm::restore(
        Snapshot::from_json(&json).unwrap(),
        build.clone(),
        missing.host(),
        Default::default(),
    )
    .unwrap();
    let new_host = TestHost::new();
    new_host.clock().advance_monotonic(Duration::from_secs(50));
    let clock = new_host.clock().clone();
    let full = || RestoreOptions {
        validation: SnapshotValidation::Full,
        test_clock: Some(clock.clone()),
        ..Default::default()
    };
    let mut edited: serde_json::Value = serde_json::from_str(&json).unwrap();
    edited["console"]["timers"][0][1]["secs"] = 1.into();
    let invalid = Snapshot::from_json(&edited.to_string()).unwrap();
    let invalid_options = full();
    assert!(
        matches!(Vm::restore(invalid, build.clone(), new_host.host(), invalid_options),
        Err(VmError::InvalidSnapshot(message)) if message.contains("console timer start"))
    );
    let options = full();
    let mut vm = Vm::restore(
        Snapshot::from_bytes(&binary).unwrap(),
        build,
        new_host.host(),
        options,
    )
    .unwrap();
    assert!(new_host.take_console_events().is_empty());
    new_host.clock().advance_monotonic(Duration::from_millis(2));
    assert_eq!(vm.start("run", vec![]).unwrap(), Value::number(42.0));
    let events = new_host.take_console_events();
    assert_eq!(
        events
            .iter()
            .map(|event| (event.level, message(event)))
            .collect::<Vec<_>>(),
        [
            (ConsoleLevel::Count, String::from("x: 2")),
            (ConsoleLevel::TimeLog, String::from("t: 3.25 ms")),
            (ConsoleLevel::TimeEnd, String::from("t: 3.25 ms")),
        ]
    );
    assert!(
        matches!(args(&events[1]), [_, ConsoleArg::Value(preview)] if preview.value == ConsoleValue::Number(7.0))
    );
    for event in events {
        let [group] = &event.groups[..] else {
            panic!("{event:?}")
        };
        assert!(group.collapsed);
        assert!(
            matches!(&group.title[..], [ConsoleArg::Value(preview)] if preview.value == ConsoleValue::String("parent".into()))
        );
    }
}

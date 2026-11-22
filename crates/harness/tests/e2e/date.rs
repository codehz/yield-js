//! Date behavior with observable host clocks, IANA zones, and snapshot restore.

use std::{cell::Cell, sync::Arc};

use yield_harness::{TestHost, build};
use yield_vm::{Bindings, Host, JsValue, PromiseState, Snapshot, Value, Vm, VmError};

struct CountingHost {
    bindings: Arc<Bindings>,
    clock_reads: Cell<usize>,
    zone_reads: Cell<usize>,
}

impl Host for CountingHost {
    fn bindings(&self) -> Arc<Bindings> {
        self.bindings.clone()
    }

    fn now_ms(&self) -> i64 {
        let reads = self.clock_reads.get();
        self.clock_reads.set(reads + 1);
        reads as i64 * 1000
    }

    fn time_zone(&self) -> &str {
        self.zone_reads.set(self.zone_reads.get() + 1);
        "Etc/UTC"
    }

    fn report_error(&mut self, error: JsValue) {
        panic!("unexpected task error: {error}");
    }
}

#[test]
fn date_implicit_now_reads_once_and_utc_paths_do_not_read_zone() {
    let mut host = CountingHost {
        bindings: TestHost::new().bindings(),
        clock_reads: Cell::new(0),
        zone_reads: Cell::new(0),
    };
    let mut vm = Vm::new(
        build(
            r#"
            export function now() { return Date.now(); }
            export function call() {
                return Date({ valueOf() { throw 'Date() must ignore its arguments'; } });
            }
            export function construct() { return new Date().getTime(); }
            export function parseUtc() { return Date.parse('1970-01-01T00:00:00.003Z'); }
            export function parseDateOnly() { return Date.parse('1970-01-02'); }
            export function utc() { return Date.UTC(1970, 0, 1, 0, 0, 0, 4); }
            export function numberArg() { return new Date(5).getTime(); }
            export function copyArg() { return new Date(new Date(6)).getTime(); }
            export function localArgs() { return new Date(1970, 0, 1, 0, 0, 0, 7).getTime(); }
            export function parseLocal() { return Date.parse('1970-01-01T00:00:00.008'); }
            export function utcMethods() {
                const d = new Date(0);
                d.setUTCSeconds(1);
                return d.toISOString() + '|' + d.getUTCSeconds();
            }
            "#,
        ),
        &mut host,
    )
    .unwrap();
    assert_eq!(host.clock_reads.get(), 0);
    assert_eq!(host.zone_reads.get(), 0);

    for (entry, expected, clock_reads, uses_zone) in [
        ("now", Value::number(0.0), 1, false),
        (
            "call",
            Value::string("Thu Jan 01 1970 00:00:01 GMT+0000".into()),
            1,
            true,
        ),
        ("construct", Value::number(2000.0), 1, false),
        ("parseUtc", Value::number(3.0), 0, false),
        ("parseDateOnly", Value::number(86_400_000.0), 0, false),
        ("utc", Value::number(4.0), 0, false),
        ("numberArg", Value::number(5.0), 0, false),
        ("copyArg", Value::number(6.0), 0, false),
        ("localArgs", Value::number(7.0), 0, true),
        ("parseLocal", Value::number(8.0), 0, true),
        (
            "utcMethods",
            Value::string("1970-01-01T00:00:01.000Z|1".into()),
            0,
            false,
        ),
    ] {
        let before_clock = host.clock_reads.get();
        let before_zone = host.zone_reads.get();
        assert_eq!(
            vm.start(&mut host, entry, vec![]).unwrap(),
            expected,
            "{entry}"
        );
        assert_eq!(
            host.clock_reads.get() - before_clock,
            clock_reads,
            "{entry}"
        );
        assert_eq!(host.zone_reads.get() > before_zone, uses_zone, "{entry}");
    }
}

#[test]
fn date_new_york_gap_overlap_and_seasonal_offsets() {
    let mut host = TestHost::new();
    host.set_time_zone("America/New_York");
    let mut vm = Vm::new(
        build(
            r#"
            export function gap() { return new Date(2024, 2, 10, 2, 30).toISOString(); }
            export function overlap() { return new Date(2024, 10, 3, 1, 30).toISOString(); }
            export function dateOnly() { return new Date('2024-01-01').toISOString(); }
            export function localDateTime() { return new Date('2024-01-01T00:00').toISOString(); }
            export function setGap() {
                const d = new Date(2024, 2, 10, 1, 30);
                const time = d.setHours(2);
                return d.toISOString() + '|' + d.getHours() + '|' + (time === d.getTime());
            }
            export function summer() { return new Date('2024-07-01').getTimezoneOffset(); }
            export function winter() { return new Date('2024-01-01').getTimezoneOffset(); }
            "#,
        ),
        &mut host,
    )
    .unwrap();
    for (entry, expected) in [
        ("gap", "2024-03-10T07:30:00.000Z"),
        ("overlap", "2024-11-03T05:30:00.000Z"),
        ("dateOnly", "2024-01-01T00:00:00.000Z"),
        ("localDateTime", "2024-01-01T05:00:00.000Z"),
        ("setGap", "2024-03-10T07:30:00.000Z|3|true"),
    ] {
        assert_eq!(
            vm.start(&mut host, entry, vec![]).unwrap(),
            Value::string(expected.into()),
            "{entry}"
        );
    }
    for (entry, offset) in [("summer", 240.0), ("winter", 300.0)] {
        assert_eq!(
            vm.start(&mut host, entry, vec![]).unwrap(),
            Value::number(offset),
            "{entry}"
        );
    }
}

#[test]
fn date_new_york_far_future_rules_and_historical_subminute_offset() {
    let mut host = TestHost::new();
    host.set_time_zone("America/New_York");
    let mut vm = Vm::new(
        build(
            r#"
            export function futureSummer() {
                const d = new Date(250000, 6, 1);
                return d.toISOString() + '|' + d.getTimezoneOffset();
            }
            export function futureWinter() {
                const d = new Date(250000, 0, 1);
                return d.toISOString() + '|' + d.getTimezoneOffset();
            }
            // New York local mean time was UTC-04:56:02 before standard time.
            export function historical() {
                const d = new Date('1880-01-01T00:00:00');
                return d.toISOString() + '|' + d.getSeconds();
            }
            export function historicalOffset() {
                return new Date('1880-01-01').getTimezoneOffset();
            }
            "#,
        ),
        &mut host,
    )
    .unwrap();
    for (entry, expected) in [
        ("futureSummer", "+250000-07-01T04:00:00.000Z|240"),
        ("futureWinter", "+250000-01-01T05:00:00.000Z|300"),
        ("historical", "1880-01-01T04:56:02.000Z|0"),
    ] {
        assert_eq!(
            vm.start(&mut host, entry, vec![]).unwrap(),
            Value::string(expected.into()),
            "{entry}"
        );
    }
    assert_eq!(
        vm.start(&mut host, "historicalOffset", vec![]).unwrap(),
        Value::number(17_762.0 / 60.0)
    );
}

#[test]
fn date_invalid_host_zone_faults_only_when_local_rules_are_needed() {
    let mut host = TestHost::new();
    host.set_time_zone("Invalid/Zone");
    let mut vm = Vm::new(
        build(
            r#"
            export function utc() {
                const d = new Date(Date.UTC(1970, 0, 1));
                d.setUTCSeconds(1);
                return Date.now() + Date.parse(d.toISOString()) + d.getUTCSeconds();
            }
            export function formatLocal() { return Date(); }
            export function getLocal() { return new Date(0).getHours(); }
            export function constructLocal() { return new Date(2024, 0, 1); }
            export function parseLocal() { return Date.parse('2024-01-01T00:00'); }
            "#,
        ),
        &mut host,
    )
    .unwrap();
    for entry in ["formatLocal", "getLocal", "constructLocal", "parseLocal"] {
        let result = vm.start(&mut host, entry, vec![]);
        assert!(
            matches!(&result, Err(VmError::HostFault(message)) if message.contains("Invalid/Zone")),
            "{entry}: {result:?}"
        );
    }
    assert_eq!(
        vm.start(&mut host, "utc", vec![]).unwrap(),
        Value::number(1001.0)
    );
}

const WAIT_WITH_DATES: &str = r#"
    import { sleep } from 'test';
    export function readClock() { return new Date().getTime(); }
    export async function run() {
        const original = new Date();
        original.child = { label: 'kept' };
        const invalid = new Date(NaN);
        await sleep(25);
        return original.getTime() + '|' + original.getHours() + '|' + original.child.label +
            '|' + invalid.toString() + '|' + (invalid.getTime() !== invalid.getTime()) +
            '|' + new Date().getTime();
    }
"#;

#[test]
fn date_snapshot_preserves_slots_and_properties_but_uses_restored_host() {
    let program = build(WAIT_WITH_DATES);
    let mut old_host = TestHost::new();
    let mut vm = Vm::new(program.clone(), &mut old_host).unwrap();
    let result = vm.start(&mut old_host, "run", vec![]).unwrap();
    old_host.advance(&mut vm, 5).unwrap();
    assert_eq!(vm.promise_state(&result).unwrap(), PromiseState::Pending);
    let json = vm
        .snapshot(std::slice::from_ref(&result))
        .unwrap()
        .to_json()
        .unwrap();
    drop(vm);
    assert!(old_host.clock().registrations().is_empty());

    let mut new_host = TestHost::new();
    new_host.clock().set_time(10_000).unwrap();
    new_host.set_time_zone("America/New_York");
    let mut vm = Vm::restore(Snapshot::from_json(&json).unwrap(), program, &mut new_host).unwrap();
    vm.start_recovered_operations().unwrap();
    assert_eq!(
        vm.start(&mut new_host, "readClock", vec![]).unwrap(),
        Value::number(10_000.0)
    );
    assert_eq!(new_host.clock().registrations().len(), 1);
    assert_eq!(new_host.clock().registrations()[0].1, 10_020);
    new_host.advance(&mut vm, 19).unwrap();
    assert_eq!(vm.promise_state(&result).unwrap(), PromiseState::Pending);
    new_host.advance(&mut vm, 1).unwrap();
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::string("0|19|kept|Invalid Date|true|10020".into()))
    );
    assert!(new_host.clock().registrations().is_empty());
}

#[test]
fn date_restore_rejects_out_of_range_serialized_time_slot() {
    const OUT_OF_RANGE: i64 = 8_640_000_000_000_001;
    let program = build("export function run() { return new Date(123); }");
    let mut host = TestHost::new();
    let mut vm = Vm::new(program.clone(), &mut host).unwrap();
    let date = vm.start(&mut host, "run", vec![]).unwrap();
    let snapshot = vm.snapshot(&[date]).unwrap();
    let mut json = serde_json::to_value(&snapshot).unwrap();
    let slot = json["objects"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter_map(|record| record["object"].get_mut("date"))
        .find_map(|date| {
            date.get_mut("time")
                .filter(|time| time.as_i64() == Some(123))
        })
        .expect("serialized Date time slot");
    *slot = serde_json::json!(OUT_OF_RANGE);
    let corrupt = Snapshot::from_json(&json.to_string()).unwrap();
    assert!(matches!(
        Vm::restore_validated(corrupt, program, &mut host),
        Err(VmError::InvalidSnapshot(message)) if message.contains("TimeClip")
    ));
}

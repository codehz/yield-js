//! End-to-end Fetch transport tests against loopback servers. Each test uses
//! real HTTP/1.1 connections and counts exactly which hops were sent.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use yield_harness::fetch::LoopbackFetchService;
use yield_harness::{TestHost, build};
use yield_vm::fetch::{FetchEnvironment, Origin};
use yield_vm::{Vm, VmOptions};

use super::fetch_http::{ScriptedResponse, Server};

fn environment(server: &Server, authorization: &str) -> FetchEnvironment {
    FetchEnvironment {
        origin: Some(Origin {
            scheme: "http".into(),
            host: "127.0.0.1".into(),
            port: server.port(),
        }),
        referrer: None,
        partition: "e2e".into(),
        authorization: authorization.into(),
    }
}

fn pump(vm: &mut Vm, service: &LoopbackFetchService) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut delivered = false;
        for token in service.tokens() {
            for event in service.take(token) {
                vm.deliver_fetch_event(token, event)
                    .map_err(|error| error.to_string())?;
                delivered = true;
            }
        }
        vm.run_until_idle().map_err(|error| error.to_string())?;
        if delivered {
            continue;
        }
        if service.tokens().is_empty() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("fetch did not finish".into());
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn run(server: &Server, script: String) -> Result<(TestHost, Vm), String> {
    let service = Arc::new(LoopbackFetchService::new([server.endpoint()]));
    let mut host = TestHost::with_fetch_service(service.clone());
    let environment = environment(server, "loopback");
    host.set_fetch_environment(environment.clone());
    let build = build(&script);
    let mut vm = Vm::new(
        build,
        host.host(),
        VmOptions {
            fetch_environment: Some(environment),
            ..Default::default()
        },
    )
    .map_err(|error| error.to_string())?;
    vm.start("run", vec![]).map_err(|error| error.to_string())?;
    pump(&mut vm, &service)?;
    Ok((host, vm))
}

fn export(vm: &Vm, name: &str) -> String {
    vm.get_export(name)
        .unwrap()
        .into_string()
        .unwrap()
        .to_utf8()
        .unwrap()
}

#[test]
fn fetch_http_round_trip_keeps_duplicate_headers_and_404_response() {
    let server = Server::new(|request| match request.path.as_str() {
        "/ok" => ScriptedResponse::new(200)
            .header("X-Dup", "first")
            .header("X-Dup", "second")
            .header("Content-Type", "text/plain")
            .body("hello"),
        "/chunked" => ScriptedResponse::new(200)
            .header("Content-Type", "text/plain")
            .chunked()
            .body("chunk-body"),
        _ => ScriptedResponse::new(404)
            .header("Content-Type", "text/plain")
            .body("missing"),
    });
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            const ok = await fetch("{ok}");
            const chunked = await fetch("{chunked}");
            const missing = await fetch("{missing}");
            result = [
                ok.status, ok.ok, await ok.text(), ok.headers.get("x-dup"),
                chunked.status, await chunked.text(),
                missing.status, missing.ok, await missing.text(),
            ].join("|");
        }}
        "#,
        ok = server.url("/ok"),
        chunked = server.url("/chunked"),
        missing = server.url("/missing"),
    );
    let (_host, vm) = run(&server, script).unwrap();
    assert_eq!(
        export(&vm, "result"),
        "200|true|hello|first, second|200|chunk-body|404|false|missing"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].path, "/ok");
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[2].path, "/missing");
}

#[test]
fn fetch_only_if_cached_miss_sends_no_connection() {
    let server = Server::new(|_| ScriptedResponse::new(200).body("unreachable"));
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            try {{
                await fetch("{}", {{ cache: "only-if-cached", mode: "same-origin" }});
                result = "resolved";
            }} catch (error) {{
                result = error instanceof TypeError ? "typeerror" : "other";
            }}
        }}
        "#,
        server.url("/cached")
    );
    let (_host, vm) = run(&server, script).unwrap();
    assert_eq!(export(&vm, "result"), "typeerror");
    assert_eq!(server.requests().len(), 0);
}

fn run_multi(servers: &[&Server], script: String) -> Result<(TestHost, Vm), String> {
    let endpoints: Vec<String> = servers.iter().map(|server| server.endpoint()).collect();
    let service = Arc::new(LoopbackFetchService::new(endpoints));
    let mut host = TestHost::with_fetch_service(service.clone());
    let environment = environment(servers[0], "loopback");
    host.set_fetch_environment(environment.clone());
    let build = build(&script);
    let mut vm = Vm::new(
        build,
        host.host(),
        VmOptions {
            fetch_environment: Some(environment),
            ..Default::default()
        },
    )
    .map_err(|error| error.to_string())?;
    vm.start("run", vec![]).map_err(|error| error.to_string())?;
    pump(&mut vm, &service)?;
    Ok((host, vm))
}

#[test]
fn fetch_cors_preflight_denial_sends_no_real_request() {
    let origin = Server::new(|_| ScriptedResponse::new(200));
    let other = Server::new(|request| {
        if request.method == "OPTIONS" {
            ScriptedResponse::new(200)
        } else {
            ScriptedResponse::new(200).body("should not run")
        }
    });
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            try {{
                await fetch("{}", {{ method: "POST", headers: {{ "x-test": "1" }}, body: "x" }});
                result = "resolved";
            }} catch (error) {{
                result = error instanceof TypeError ? "typeerror" : "other";
            }}
        }}
        "#,
        other.url("/target")
    );
    let (_host, vm) = run_multi(&[&origin, &other], script).unwrap();
    assert_eq!(export(&vm, "result"), "typeerror");
    let requests = other.requests();
    assert_eq!(requests.len(), 1, "only the preflight may be sent");
    assert_eq!(requests[0].method, "OPTIONS");
    assert_eq!(
        requests[0].header("access-control-request-method"),
        Some("POST")
    );
    assert_eq!(origin.requests().len(), 0);
}

#[test]
fn fetch_cors_preflight_success_allows_real_request() {
    let origin = Server::new(|_| ScriptedResponse::new(200));
    let allowed = format!("http://127.0.0.1:{}", origin.port());
    let other = Server::new(move |request| match request.method.as_str() {
        "OPTIONS" => ScriptedResponse::new(204)
            .header("Access-Control-Allow-Origin", &allowed)
            .header("Access-Control-Allow-Methods", "POST")
            .header("Access-Control-Allow-Headers", "x-test")
            .header("Access-Control-Max-Age", "60"),
        _ => ScriptedResponse::new(200)
            .header("Access-Control-Allow-Origin", &allowed)
            .header("Access-Control-Expose-Headers", "x-exposed")
            .header("X-Exposed", "visible")
            .header("X-Hidden", "secret")
            .body("cors-body"),
    });
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            try {{
                const response = await fetch("{}", {{ method: "POST", headers: {{ "x-test": "1" }}, body: "x" }});
                result = [response.status, response.type, await response.text(),
                    response.headers.get("x-exposed"), response.headers.get("x-hidden")].join("|");
            }} catch (error) {{ result = "ERR:" + error.name + ":" + error.message; }}
        }}
        "#,
        other.url("/target")
    );
    let (_host, vm) = run_multi(&[&origin, &other], script).unwrap();
    assert_eq!(export(&vm, "result"), "200|cors|cors-body|visible|");
    let requests = other.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "OPTIONS");
    assert_eq!(requests[1].method, "POST");
}

#[test]
fn fetch_cross_origin_redirect_strips_authorization() {
    let origin0 = Server::new(|_| ScriptedResponse::new(200));
    let _ = origin0.port();
    let other = Server::new(|_| {
        ScriptedResponse::new(200)
            .header("Access-Control-Allow-Origin", "*")
            .body("target")
    });
    // Rewrite the first redirect to point at the other server.
    let redirected = other.url("/target");
    let origin = Server::new(move |request| match request.path.as_str() {
        "/start" => ScriptedResponse::new(302).header("Location", &redirected),
        _ => ScriptedResponse::new(200).body("origin"),
    });
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            try {{
                const response = await fetch("{}", {{ headers: {{ authorization: "Bearer secret" }} }});
                result = response.status + ":" + (await response.text()) + ":" + response.redirected;
            }} catch (error) {{ result = "ERR:" + error.name + ":" + error.message; }}
        }}
        "#,
        origin.url("/start")
    );
    let (_host, vm) = run_multi(&[&origin, &other], script).unwrap();
    assert_eq!(export(&vm, "result"), "200:target:true");
    let requests = other.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("authorization"), None);
    assert_eq!(
        origin.requests()[0].header("authorization"),
        Some("Bearer secret")
    );
}

#[test]
fn fetch_307_streaming_body_is_not_replayed() {
    let server = Server::new(|request| match request.path.as_str() {
        "/start" => ScriptedResponse::new(307).header("Location", "/target"),
        _ => ScriptedResponse::new(200).body("target"),
    });
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            const body = new ReadableStream({{
                start(controller) {{ controller.enqueue(new Uint8Array([1, 2, 3])); controller.close(); }},
            }});
            const request = new Request("{start}", {{ method: "POST", body, duplex: "half" }});
            try {{
                await fetch(request);
                result = "resolved";
            }} catch (error) {{
                result = error instanceof TypeError ? "typeerror" : "other";
            }}
        }}
        "#,
        start = server.url("/start")
    );
    let (_host, vm) = run(&server, script).unwrap();
    assert_eq!(export(&vm, "result"), "typeerror");
    assert_eq!(server.requests().len(), 1, "no replay after a 307");
}

#[test]
fn fetch_integrity_verification_gates_exposure() {
    let server = Server::new(|_| {
        ScriptedResponse::new(200)
            .header("Content-Type", "text/plain")
            .body("hello")
    });
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            const good = await fetch("{url}", {{ integrity: "sha256-LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=" }});
            const body = await good.text();
            try {{
                await fetch("{url}", {{ integrity: "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=" }});
                result = "bad-resolved";
            }} catch (error) {{
                result = body + ":" + (error instanceof TypeError ? "typeerror" : "other");
            }}
        }}
        "#,
        url = server.url("/body")
    );
    let (_host, vm) = run(&server, script).unwrap();
    assert_eq!(export(&vm, "result"), "hello:typeerror");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn fetch_decodes_gzip_once() {
    let gzip: Vec<u8> = vec![
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0x57, 0x48, 0xaf, 0xca, 0x2c, 0x00, 0x00, 0x19, 0x6a, 0xd2, 0xdf, 0x0a, 0x00, 0x00, 0x00,
    ];
    let server = Server::new(move |_| {
        ScriptedResponse::new(200)
            .header("Content-Type", "text/plain")
            .header("Content-Encoding", "gzip")
            .body(gzip.clone())
    });
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            const response = await fetch("{}");
            result = (await response.text()) + ":" + response.headers.get("content-encoding");
        }}
        "#,
        server.url("/gzip")
    );
    let (_host, vm) = run(&server, script).unwrap();
    assert_eq!(export(&vm, "result"), "hello gzip:gzip");
}

#[test]
fn fetch_streaming_upload_sends_only_granted_bytes() {
    let server =
        Server::new(|request| ScriptedResponse::new(200).body(request.body.len().to_string()));
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            const body = new ReadableStream({{
                start(controller) {{
                    controller.enqueue(new Uint8Array([1, 2, 3, 4, 5]));
                    controller.close();
                }},
            }});
            const response = await fetch("{}", {{ method: "POST", body, duplex: "half" }});
            result = response.status + ":" + (await response.text());
        }}
        "#,
        server.url("/upload")
    );
    let (_host, vm) = run(&server, script).unwrap();
    assert_eq!(export(&vm, "result"), "200:5");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body, vec![1, 2, 3, 4, 5]);
}

#[test]
fn fetch_keepalive_budget_rejects_oversized_body() {
    let server = Server::new(|_| ScriptedResponse::new(200));
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            try {{
                await fetch("{}", {{ method: "POST", body: "x".repeat(65537), keepalive: true }});
                result = "resolved";
            }} catch (error) {{
                result = error instanceof TypeError ? "typeerror" : "other";
            }}
        }}
        "#,
        server.url("/large")
    );
    let (_host, vm) = run(&server, script).unwrap();
    assert_eq!(export(&vm, "result"), "typeerror");
    assert_eq!(server.requests().len(), 0);
}

#[test]
fn fetch_abort_preserves_the_script_reason() {
    let server = Server::new(|_| ScriptedResponse::new(200).body("late"));
    let script = format!(
        r#"
        export let result = "";
        export async function run() {{
            const controller = new AbortController();
            const reason = {{ marker: "abort" }};
            const promise = fetch("{}", {{ signal: controller.signal }});
            controller.abort(reason);
            try {{
                await promise;
                result = "resolved";
            }} catch (error) {{
                result = error === reason ? "reason" : "other";
            }}
        }}
        "#,
        server.url("/slow")
    );
    let (_host, vm) = run(&server, script).unwrap();
    assert_eq!(export(&vm, "result"), "reason");
}

mod snapshot {
    use std::sync::Arc;

    use yield_harness::fetch::{ControllableFetchService, FetchCall};
    use yield_harness::{TestHost, build};
    use yield_vm::binding::OperationToken;
    use yield_vm::fetch::{FetchEnvironment, FetchEvent, ResponseHead};
    use yield_vm::{RestoreOptions, SnapshotValidation, Vm, VmOptions};

    fn environment() -> FetchEnvironment {
        FetchEnvironment {
            authorization: "controlled".into(),
            partition: "snapshot".into(),
            ..Default::default()
        }
    }

    fn head(body: &str) -> ResponseHead {
        let mut headers = vec![
            (b"content-type".to_vec(), b"text/plain".to_vec()),
            (b"access-control-allow-origin".to_vec(), b"*".to_vec()),
        ];
        if body.is_empty() {
            headers.push((b"content-length".to_vec(), b"0".to_vec()));
        }
        ResponseHead {
            status: 200,
            status_text: "OK".into(),
            headers,
        }
    }

    fn start(service: &Arc<ControllableFetchService>) -> (TestHost, Vm) {
        start_with(service, SCRIPT)
    }

    fn start_with(service: &Arc<ControllableFetchService>, source: &str) -> (TestHost, Vm) {
        let mut host = TestHost::with_fetch_service(service.clone());
        let environment = environment();
        host.set_fetch_environment(environment.clone());
        let build = build(source);
        let mut vm = Vm::new(
            build,
            host.host(),
            VmOptions {
                fetch_environment: Some(environment),
                ..Default::default()
            },
        )
        .unwrap();
        vm.start("run", vec![]).unwrap();
        vm.run_until_idle().unwrap();
        (host, vm)
    }

    fn token(service: &Arc<ControllableFetchService>) -> OperationToken {
        let calls = service.take_calls();
        let Some(FetchCall::Start(token, _)) = calls.first() else {
            panic!("missing fetch start: {calls:?}");
        };
        *token
    }

    fn deliver(
        vm: &mut Vm,
        service: &Arc<ControllableFetchService>,
        token: OperationToken,
        event: FetchEvent,
    ) {
        service.push(token, event).unwrap();
        for event in service.take(token) {
            vm.deliver_fetch_event(token, event).unwrap();
        }
        vm.run_until_idle().unwrap();
    }

    fn outcome(vm: &Vm) -> String {
        vm.get_export("outcome")
            .unwrap()
            .into_string()
            .unwrap()
            .to_utf8()
            .unwrap()
    }

    fn restore(vm: &Vm, build_source: &str, host: &TestHost) -> Vm {
        let snapshot = vm.snapshot(&[]).unwrap();
        let snapshot = yield_vm::Snapshot::from_bytes(&snapshot.to_bytes().unwrap()).unwrap();
        Vm::restore(
            snapshot,
            build(build_source),
            host.host(),
            RestoreOptions {
                validation: SnapshotValidation::Full,
                fetch_environment: Some(environment()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    const SCRIPT: &str = r#"
        export let outcome = "pending";
        export async function run() {
            try {
                const response = await fetch("http://fetch.test/data");
                outcome = "head:" + response.status;
                try {
                    const text = await response.text();
                    outcome = "body:" + text;
                } catch (error) { outcome = "stream:" + error.name; }
            } catch (error) { outcome = "rejected:" + error.name; }
        }
    "#;

    #[test]
    fn restored_awaiting_head_rejects_with_abort_error_without_replaying() {
        let service = Arc::new(ControllableFetchService::default());
        let (_host, mut vm) = start(&service);
        let token = token(&service);
        let restored_host = TestHost::default();
        let mut restored = restore(&vm, SCRIPT, &restored_host);
        assert_eq!(outcome(&restored), "pending");
        // The old generation token is inert after restore.
        restored
            .deliver_fetch_event(token, FetchEvent::Head(head("")))
            .unwrap();
        restored.run_until_idle().unwrap();
        assert_eq!(outcome(&restored), "rejected:AbortError");

        // The source VM keeps its registration and settles normally.
        deliver(&mut vm, &service, token, FetchEvent::Head(head("abc")));
        deliver(&mut vm, &service, token, FetchEvent::Data(b"abc".to_vec()));
        deliver(&mut vm, &service, token, FetchEvent::Eof);
        assert_eq!(outcome(&vm), "body:abc");
    }

    #[test]
    fn restored_streaming_response_errors_only_the_body() {
        let service = Arc::new(ControllableFetchService::default());
        let (_host, mut vm) = start(&service);
        let token = token(&service);
        deliver(&mut vm, &service, token, FetchEvent::Head(head("")));
        assert_eq!(outcome(&vm), "head:200");
        let restored_host = TestHost::default();
        let mut restored = restore(&vm, SCRIPT, &restored_host);
        restored.run_until_idle().unwrap();
        assert_eq!(outcome(&restored), "stream:AbortError");
    }

    #[test]
    fn completed_bodies_stay_readable_after_restore() {
        let service = Arc::new(ControllableFetchService::default());
        let (_host, mut vm) = start(&service);
        let token = token(&service);
        deliver(&mut vm, &service, token, FetchEvent::Head(head("abc")));
        deliver(&mut vm, &service, token, FetchEvent::Data(b"abc".to_vec()));
        deliver(&mut vm, &service, token, FetchEvent::Eof);
        assert_eq!(outcome(&vm), "body:abc");
        let restored_host = TestHost::default();
        let mut restored = restore(&vm, SCRIPT, &restored_host);
        restored.run_until_idle().unwrap();
        assert_eq!(outcome(&restored), "body:abc");
    }

    #[test]
    fn full_validation_rejects_a_duplicated_queued_head() {
        let service = Arc::new(ControllableFetchService::default());
        let (_host, mut vm) = start(&service);
        let token = token(&service);
        service.push(token, FetchEvent::Head(head(""))).unwrap();
        for event in service.take(token) {
            vm.deliver_fetch_event(token, event).unwrap();
        }
        let snapshot = vm.snapshot(&[]).unwrap();
        let mut json = serde_json::to_value(&snapshot).unwrap();
        let tasks = json["ordinary_tasks"].as_array_mut().unwrap();
        let index = tasks
            .iter()
            .position(|task| task.get("FetchEvent").is_some())
            .expect("queued fetch event");
        let duplicate = tasks[index].clone();
        tasks.insert(index, duplicate);
        let corrupted: yield_vm::Snapshot = serde_json::from_value(json).unwrap();
        let host = TestHost::default();
        assert!(matches!(
            Vm::restore(
                corrupted,
                build(SCRIPT),
                host.host(),
                RestoreOptions {
                    validation: SnapshotValidation::Full,
                    fetch_environment: Some(environment()),
                    ..Default::default()
                },
            ),
            Err(yield_vm::VmError::InvalidSnapshot(message)) if message.contains("fetch")
        ));
    }
    const UPLOAD_SCRIPT: &str = r#"
        export let outcome = "pending";
        export async function run() {
            const body = new ReadableStream({
                start(controller) {
                    controller.enqueue(new Uint8Array([1, 2, 3, 4, 5]));
                    controller.close();
                },
            });
            try {
                const response = await fetch("http://fetch.test/upload", {
                    method: "POST", body, duplex: "half",
                });
                outcome = "head:" + response.status + ":body:" + (await response.text());
            } catch (error) { outcome = "rejected:" + error.name; }
        }
    "#;

    fn accept(
        vm: &mut Vm,
        service: &Arc<ControllableFetchService>,
        token: OperationToken,
        event: FetchEvent,
    ) {
        service.push(token, event).unwrap();
        for event in service.take(token) {
            vm.deliver_fetch_event(token, event).unwrap();
        }
    }

    #[test]
    fn restored_mid_upload_interrupts_without_replaying() {
        let service = Arc::new(ControllableFetchService::default());
        let (_host, mut vm) = start_with(&service, UPLOAD_SCRIPT);
        let token = token(&service);
        accept(&mut vm, &service, token, FetchEvent::UploadCredit(2));
        vm.run_until_idle().unwrap();
        let mut uploaded = 0;
        for call in service.take_calls() {
            if let FetchCall::SendUpload(_, bytes) = call {
                assert!(bytes.len() <= 2, "upload exceeded granted credit");
                uploaded += bytes.len();
            }
        }
        assert_eq!(uploaded, 2, "only granted bytes are sent");

        let restored_service = Arc::new(ControllableFetchService::default());
        let restored_host = TestHost::with_fetch_service(restored_service.clone());
        let mut restored = restore(&vm, UPLOAD_SCRIPT, &restored_host);
        restored.run_until_idle().unwrap();
        assert_eq!(outcome(&restored), "rejected:AbortError");
        assert!(
            restored_service.take_calls().is_empty(),
            "restore must not start transport"
        );

        accept(&mut vm, &service, token, FetchEvent::UploadCredit(16));
        vm.run_until_idle().unwrap();
        accept(&mut vm, &service, token, FetchEvent::Head(head("")));
        accept(&mut vm, &service, token, FetchEvent::Eof);
        vm.run_until_idle().unwrap();
        assert_eq!(outcome(&vm), "head:200:body:");
    }

    #[test]
    fn accepted_eof_drains_before_the_interruption() {
        let service = Arc::new(ControllableFetchService::default());
        let (_host, mut vm) = start_with(&service, SCRIPT);
        let token = token(&service);
        deliver(&mut vm, &service, token, FetchEvent::Head(head("abc")));
        assert_eq!(outcome(&vm), "head:200");
        accept(&mut vm, &service, token, FetchEvent::Data(b"abc".to_vec()));
        accept(&mut vm, &service, token, FetchEvent::Eof);

        let restored_service = Arc::new(ControllableFetchService::default());
        let restored_host = TestHost::with_fetch_service(restored_service.clone());
        let mut restored = restore(&vm, SCRIPT, &restored_host);
        restored.run_until_idle().unwrap();
        assert_eq!(outcome(&restored), "body:abc");
        assert!(restored_service.take_calls().is_empty());
    }

    #[test]
    fn host_protocol_errors_reject_out_of_order_and_excess_events() {
        let service = Arc::new(ControllableFetchService::default());
        let (_host, mut vm) = start(&service);
        let token = token(&service);
        service.push(token, FetchEvent::Data(vec![0])).unwrap();
        let event = service.take(token).pop().unwrap();
        assert!(
            vm.deliver_fetch_event(token, event).is_err(),
            "data before head"
        );
        service.push(token, FetchEvent::Head(head(""))).unwrap();
        for event in service.take(token) {
            vm.deliver_fetch_event(token, event).unwrap();
        }
        service
            .push(token, FetchEvent::Data(vec![0; 65537]))
            .unwrap();
        let event = service.take(token).pop().unwrap();
        assert!(
            vm.deliver_fetch_event(token, event).is_err(),
            "excess download credit"
        );
    }

    #[test]
    fn failure_after_head_only_errors_the_body() {
        let service = Arc::new(ControllableFetchService::default());
        let (_host, mut vm) = start(&service);
        let token = token(&service);
        deliver(&mut vm, &service, token, FetchEvent::Head(head("")));
        assert_eq!(outcome(&vm), "head:200");
        deliver(
            &mut vm,
            &service,
            token,
            FetchEvent::Failed(yield_vm::fetch::FetchFailure::Network("boom".into())),
        );
        assert_eq!(outcome(&vm), "stream:TypeError");
    }
}

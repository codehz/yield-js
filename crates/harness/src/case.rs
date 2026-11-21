use yield_vm::{Completion, Host, JsString, JsValue, PromiseState, Snapshot, Value, ValueRef, Vm};

use crate::{TestHost, compile_source};

/// Runs one directive-headed JavaScript case against a fresh [`TestHost`].
///
/// The header is the leading run of blank lines and `// @` directives. The
/// whole source, header included, is compiled unchanged so diagnostics keep
/// the file's line numbers.
/// String payloads accept JSON strings or `{ "utf16": [...] }`; host operation
/// IDs in directives must still be JSON strings.
pub fn run_case(name: &str, source: &str) -> Result<(), String> {
    let mut steps = parse_header(source).map_err(|error| prefix_file(name, &error))?;
    let args = match steps.first() {
        Some(Step {
            kind: Directive::Args(values),
            ..
        }) => {
            let values = values.clone();
            steps.remove(0);
            values
        }
        _ => Vec::new(),
    };

    let build = compile_source(source).map_err(|error| format!("{name}: compile: {error}"))?;
    let mut host = TestHost::default();
    let mut vm =
        Vm::new(build.clone(), &mut host).map_err(|error| format!("{name}: vm: {error}"))?;
    let result = vm
        .start(&mut host, "run", args)
        .map_err(|error| format!("{name}: start: {}", format_vm_error(&error)))?;
    host.set_gc_roots(vec![result]);
    vm.run_until_idle(&mut host)
        .map_err(|error| format!("{name}: drain: {error}"))?;

    for step in steps {
        match step.kind {
            Directive::Args(_) => {
                return Err(format!(
                    "{name}:{}: @args: must be the first directive",
                    step.line
                ));
            }
            Directive::Restore => restore(name, step.line, &mut vm, &build, &mut host)?,
            Directive::Advance(ms) => host
                .advance(&mut vm, ms)
                .map_err(|error| located(name, step.line, "@advance", error))?,
            Directive::Fulfill(id, value) => {
                complete(
                    name,
                    step.line,
                    "@fulfill",
                    &mut vm,
                    &mut host,
                    &id,
                    Completion::Fulfilled(value),
                )?;
            }
            Directive::Reject(id, value) => {
                complete(
                    name,
                    step.line,
                    "@reject",
                    &mut vm,
                    &mut host,
                    &id,
                    Completion::Rejected(value),
                )?;
            }
            Directive::Pending(expected) => {
                assert_pending(name, step.line, &host, &expected)?;
            }
            Directive::Returns(expected) => {
                assert_value(name, step.line, "@returns", &expected, &host.gc_roots()[0])?;
            }
            Directive::Fulfilled(expected) => {
                assert_settled(
                    name,
                    step.line,
                    "@fulfilled",
                    &vm,
                    &host.gc_roots()[0],
                    true,
                    &expected,
                )?;
            }
            Directive::Rejected(expected) => {
                assert_settled(
                    name,
                    step.line,
                    "@rejected",
                    &vm,
                    &host.gc_roots()[0],
                    false,
                    &expected,
                )?;
            }
            Directive::Uncaught(expected) => {
                assert_uncaught(name, step.line, &expected, host.errors())?;
            }
        }
    }
    Ok(())
}

struct Step {
    line: usize,
    kind: Directive,
}

enum Directive {
    Args(Vec<JsValue>),
    Restore,
    Advance(u64),
    Fulfill(String, JsValue),
    Reject(String, JsValue),
    Pending(Vec<String>),
    Returns(Expected),
    Fulfilled(Expected),
    Rejected(Expected),
    Uncaught(Vec<Expected>),
}

enum Expected {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
}

fn parse_header(source: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (index, raw) in source.split('\n').enumerate() {
        let line_no = index + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() {
            continue;
        }
        let Some(body) = line.trim_start().strip_prefix("// @") else {
            break;
        };
        steps.push(parse_directive(line_no, body)?);
    }
    validate(&steps)?;
    Ok(steps)
}

fn parse_directive(line: usize, body: &str) -> Result<Step, String> {
    let body = body.trim();
    let (name, args) = split_command(body);
    if name.is_empty() {
        return Err(format!("{line}: unknown directive"));
    }
    let kind = match name {
        "args" => Directive::Args(parse_args(line, args)?),
        "restore" => {
            require_no_args(line, "@restore", args)?;
            Directive::Restore
        }
        "advance" => Directive::Advance(parse_advance(line, args)?),
        "fulfill" => {
            let (id, value) = parse_completion(line, "@fulfill", args)?;
            Directive::Fulfill(id, value)
        }
        "reject" => {
            let (id, value) = parse_completion(line, "@reject", args)?;
            Directive::Reject(id, value)
        }
        "pending" => Directive::Pending(parse_pending(line, args)?),
        "returns" => Directive::Returns(parse_expected(line, "@returns", args)?),
        "fulfilled" => Directive::Fulfilled(parse_expected(line, "@fulfilled", args)?),
        "rejected" => Directive::Rejected(parse_expected(line, "@rejected", args)?),
        "uncaught" => Directive::Uncaught(parse_uncaught(line, args)?),
        other => return Err(format!("{line}: unknown directive `{other}`")),
    };
    Ok(Step { line, kind })
}

fn split_command(body: &str) -> (&str, &str) {
    match body.find(char::is_whitespace) {
        Some(index) => {
            let (name, rest) = body.split_at(index);
            (name, rest.trim())
        }
        None => (body, ""),
    }
}

fn validate(steps: &[Step]) -> Result<(), String> {
    if steps.is_empty() {
        return Err("missing terminal assertion".to_owned());
    }
    for (index, step) in steps.iter().enumerate() {
        if matches!(step.kind, Directive::Args(_)) && index != 0 {
            return Err(format!("{}: @args: must be the first directive", step.line));
        }
        if is_terminal(&step.kind) && index + 1 != steps.len() {
            return Err(format!(
                "{}: {}: terminal assertion must be the last directive",
                step.line,
                directive_name(&step.kind)
            ));
        }
    }
    let last = steps.last().expect("non-empty");
    if !is_terminal(&last.kind) {
        return Err(format!(
            "{}: {}: missing terminal assertion",
            last.line,
            directive_name(&last.kind)
        ));
    }
    Ok(())
}

fn is_terminal(kind: &Directive) -> bool {
    matches!(
        kind,
        Directive::Returns(_)
            | Directive::Fulfilled(_)
            | Directive::Rejected(_)
            | Directive::Uncaught(_)
    )
}

fn directive_name(kind: &Directive) -> &'static str {
    match kind {
        Directive::Args(_) => "@args",
        Directive::Restore => "@restore",
        Directive::Advance(_) => "@advance",
        Directive::Fulfill(_, _) => "@fulfill",
        Directive::Reject(_, _) => "@reject",
        Directive::Pending(_) => "@pending",
        Directive::Returns(_) => "@returns",
        Directive::Fulfilled(_) => "@fulfilled",
        Directive::Rejected(_) => "@rejected",
        Directive::Uncaught(_) => "@uncaught",
    }
}

fn require_no_args(line: usize, directive: &str, args: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("{line}: {directive}: unexpected argument"))
    }
}

fn require_args<'a>(line: usize, directive: &str, args: &'a str) -> Result<&'a str, String> {
    if args.is_empty() {
        Err(format!("{line}: {directive}: missing argument"))
    } else {
        Ok(args)
    }
}

fn parse_advance(line: usize, args: &str) -> Result<u64, String> {
    let args = require_args(line, "@advance", args)?;
    if !args.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "{line}: @advance: expected a non-negative decimal integer"
        ));
    }
    args.parse()
        .map_err(|_| format!("{line}: @advance: value exceeds u64::MAX"))
}

fn parse_args(line: usize, args: &str) -> Result<Vec<JsValue>, String> {
    let items = parse_array(line, "@args", args)?;
    items
        .into_iter()
        .map(|item| host_scalar(line, "@args", item))
        .collect()
}

fn parse_pending(line: usize, args: &str) -> Result<Vec<String>, String> {
    let items = parse_array(line, "@pending", args)?;
    items
        .into_iter()
        .map(|item| match item {
            serde_json::Value::String(id) => Ok(id),
            _ => Err(format!(
                "{line}: @pending: expected a string, found {}",
                json_kind(&item)
            )),
        })
        .collect()
}

fn parse_completion(line: usize, directive: &str, args: &str) -> Result<(String, JsValue), String> {
    let items = parse_array(line, directive, args)?;
    if items.len() != 2 {
        return Err(format!(
            "{line}: {directive}: expected exactly two items, found {}",
            items.len()
        ));
    }
    let mut items = items.into_iter();
    let id = match items.next().expect("checked directive arity") {
        serde_json::Value::String(id) => id,
        other => {
            return Err(format!(
                "{line}: {directive}: operation id must be a string, found {}",
                json_kind(&other)
            ));
        }
    };
    let value = host_scalar(
        line,
        directive,
        items.next().expect("checked directive arity"),
    )?;
    Ok((id, value))
}

fn parse_array(line: usize, directive: &str, args: &str) -> Result<Vec<serde_json::Value>, String> {
    let args = require_args(line, directive, args)?;
    let value: serde_json::Value = serde_json::from_str(args)
        .map_err(|error| format!("{line}: {directive}: invalid argument: {error}"))?;
    match value {
        serde_json::Value::Array(items) => Ok(items),
        other => Err(format!(
            "{line}: {directive}: expected a JSON array, found {}",
            json_kind(&other)
        )),
    }
}

fn parse_uncaught(line: usize, args: &str) -> Result<Vec<Expected>, String> {
    const DIRECTIVE: &str = "@uncaught";
    let mut undefined = Vec::new();
    let mut normalized = String::new();
    let args = if let Some(items) = args.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
        && items.contains("undefined")
    {
        // Normalize whole bare undefined items without changing quoted strings.
        let mut quoted = false;
        let mut escaped = false;
        let mut depth = 0;
        let items = items.split(|ch| {
            if escaped {
                escaped = false;
            } else if quoted && ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = !quoted;
            } else if !quoted {
                match ch {
                    '{' | '[' => depth += 1,
                    '}' | ']' => depth -= 1,
                    ',' if depth == 0 => return true,
                    _ => {}
                }
            }
            false
        });
        normalized.push('[');
        for (index, item) in items.enumerate() {
            if index != 0 {
                normalized.push(',');
            }
            if item.trim() == "undefined" {
                undefined.push(index);
                normalized.push_str("null");
            } else {
                normalized.push_str(item);
            }
        }
        normalized.push(']');
        &normalized
    } else {
        args
    };
    let mut undefined = undefined.into_iter().peekable();
    parse_array(line, DIRECTIVE, args)?
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            if undefined.next_if_eq(&index).is_some() {
                Ok(Expected::Undefined)
            } else {
                json_scalar(line, DIRECTIVE, value)
            }
        })
        .collect()
}

fn parse_expected(line: usize, directive: &str, args: &str) -> Result<Expected, String> {
    let args = require_args(line, directive, args)?;
    if args == "undefined" {
        return Ok(Expected::Undefined);
    }
    let value: serde_json::Value = serde_json::from_str(args)
        .map_err(|error| format!("{line}: {directive}: invalid argument: {error}"))?;
    json_scalar(line, directive, value)
}

fn host_scalar(line: usize, directive: &str, value: serde_json::Value) -> Result<JsValue, String> {
    Ok(match json_scalar(line, directive, value)? {
        Expected::Undefined => JsValue::undefined(),
        Expected::Null => JsValue::null(),
        Expected::Bool(value) => JsValue::boolean(value),
        Expected::Number(value) => JsValue::number(value),
        Expected::String(value) => JsValue::string(value),
    })
}

fn json_scalar(line: usize, directive: &str, value: serde_json::Value) -> Result<Expected, String> {
    match value {
        serde_json::Value::Null => Ok(Expected::Null),
        serde_json::Value::Bool(value) => Ok(Expected::Bool(value)),
        serde_json::Value::Number(value) => {
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| {
                    format!("{line}: {directive}: non-finite numbers are not allowed")
                })?;
            Ok(Expected::Number(number))
        }
        serde_json::Value::String(value) => Ok(Expected::String(JsString::from(value))),
        serde_json::Value::Array(_) => Err(format!(
            "{line}: {directive}: nested arrays are not allowed"
        )),
        serde_json::Value::Object(fields) if fields.contains_key("utf16") => {
            serde_json::from_value(serde_json::Value::Object(fields))
                .map(Expected::String)
                .map_err(|error| format!("{line}: {directive}: invalid UTF-16 string: {error}"))
        }
        serde_json::Value::Object(_) => {
            Err(format!("{line}: {directive}: objects are not allowed"))
        }
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn restore(
    name: &str,
    line: usize,
    vm: &mut Vm,
    build: &yield_vm::bytecode::Build,
    host: &mut TestHost,
) -> Result<(), String> {
    let snapshot = vm
        .snapshot(host.gc_roots())
        .map_err(|error| located(name, line, "@restore", error))?;
    // Round-trip the codec. Restoring the in-memory snapshot would skip it.
    let json = snapshot
        .to_json()
        .map_err(|error| located(name, line, "@restore", error))?;
    let snapshot =
        Snapshot::from_json(&json).map_err(|error| located(name, line, "@restore", error))?;
    *vm = Vm::restore(snapshot, build.clone(), host)
        .map_err(|error| located(name, line, "@restore", error))?;
    // The snapshot includes the host's root graph and restore preserves its
    // object and symbol IDs, so the retained values now refer to that graph.
    vm.start_recovered_operations()
        .map_err(|error| located(name, line, "@restore", error))?;
    vm.run_until_idle(host)
        .map_err(|error| located(name, line, "@restore", error))?;
    Ok(())
}

fn complete(
    name: &str,
    line: usize,
    directive: &str,
    vm: &mut Vm,
    host: &mut TestHost,
    id: &str,
    completion: Completion,
) -> Result<(), String> {
    host.complete(vm, id, completion)
        .map_err(|error| located(name, line, directive, error))?;
    vm.run_until_idle(host)
        .map_err(|error| located(name, line, directive, error))?;
    Ok(())
}

fn assert_pending(
    name: &str,
    line: usize,
    host: &TestHost,
    expected: &[String],
) -> Result<(), String> {
    let mut expected = expected.to_vec();
    let mut actual = host.pending_labels();
    expected.sort();
    actual.sort();
    if expected != actual {
        return Err(format!(
            "{name}:{line}: @pending: expected {expected:?}, actual {actual:?}"
        ));
    }
    Ok(())
}

fn assert_settled(
    name: &str,
    line: usize,
    directive: &str,
    vm: &Vm,
    result: &Value,
    fulfilled: bool,
    expected: &Expected,
) -> Result<(), String> {
    let state = vm
        .promise_state(result)
        .map_err(|error| located(name, line, directive, error))?;
    let payload = match (fulfilled, &state) {
        (true, PromiseState::Fulfilled(value)) => value,
        (false, PromiseState::Rejected(value)) => value,
        _ => {
            return Err(format!(
                "{name}:{line}: {directive}: expected {}, actual {}",
                if fulfilled { "fulfilled" } else { "rejected" },
                format_state(&state)
            ));
        }
    };
    assert_value(name, line, directive, expected, payload)
}

fn assert_value(
    name: &str,
    line: usize,
    directive: &str,
    expected: &Expected,
    actual: &Value,
) -> Result<(), String> {
    if values_equal(expected, actual) {
        return Ok(());
    }
    Err(format!(
        "{name}:{line}: {directive}: expected {}, actual {}",
        format_expected(expected),
        format_value(actual)
    ))
}

fn assert_uncaught(
    name: &str,
    line: usize,
    expected: &[Expected],
    actual: &[JsValue],
) -> Result<(), String> {
    if expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual)
            .all(|(expected, actual)| values_equal_js(expected, actual))
    {
        return Ok(());
    }
    Err(format!(
        "{name}:{line}: @uncaught: expected [{}], actual [{}]",
        expected
            .iter()
            .map(format_expected)
            .collect::<Vec<_>>()
            .join(", "),
        actual
            .iter()
            .map(JsValue::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    ))
}

fn values_equal_js(expected: &Expected, actual: &JsValue) -> bool {
    match expected {
        Expected::Undefined => actual.is_undefined(),
        Expected::Null => actual.is_null(),
        Expected::Bool(expected) => actual.as_bool() == Some(*expected),
        Expected::Number(expected) => actual.as_number() == Some(*expected),
        Expected::String(expected) => actual.as_string() == Some(expected),
    }
}

fn values_equal(expected: &Expected, actual: &Value) -> bool {
    match (expected, actual.view()) {
        (Expected::Undefined, ValueRef::Undefined) => true,
        (Expected::Null, ValueRef::Null) => true,
        (Expected::Bool(expected), ValueRef::Bool(actual)) => *expected == actual,
        (Expected::Number(expected), ValueRef::Number(actual)) => *expected == actual,
        (Expected::String(expected), ValueRef::String(actual)) => expected == actual,
        _ => false,
    }
}

fn format_state(state: &PromiseState) -> String {
    match state {
        PromiseState::Pending => "pending".to_owned(),
        PromiseState::Fulfilled(value) => format!("fulfilled {}", format_value(value)),
        PromiseState::Rejected(value) => format!("rejected {}", format_value(value)),
    }
}

fn format_expected(value: &Expected) -> String {
    match value {
        Expected::Undefined => "undefined".to_owned(),
        Expected::Null => "null".to_owned(),
        Expected::Bool(value) => value.to_string(),
        Expected::Number(value) => format_number(*value),
        Expected::String(value) => format!("{value:?}"),
    }
}

fn format_value(value: &Value) -> String {
    match value.view() {
        ValueRef::Undefined => "undefined".to_owned(),
        ValueRef::Null => "null".to_owned(),
        ValueRef::Bool(value) => value.to_string(),
        ValueRef::Number(value) => format_number(value),
        ValueRef::String(value) => value
            .to_utf8()
            .map(|text| format!("{text:?}"))
            .unwrap_or_else(|_| format!("{value:?}")),
        ValueRef::Symbol(_) => "symbol".to_owned(),
        ValueRef::Object(_) => "object".to_owned(),
    }
}

fn format_vm_error(error: &yield_vm::VmError) -> String {
    match error {
        yield_vm::VmError::Thrown(value) => format_value(value),
        other => other.to_string(),
    }
}

fn format_number(value: f64) -> String {
    if value.is_finite() {
        value.to_string()
    } else if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_sign_positive() {
        "Infinity".to_owned()
    } else {
        "-Infinity".to_owned()
    }
}

fn located(name: &str, line: usize, directive: &str, error: impl std::fmt::Display) -> String {
    format!("{name}:{line}: {directive}: {error}")
}

fn prefix_file(name: &str, error: &str) -> String {
    if error
        .chars()
        .next()
        .is_some_and(|char| char.is_ascii_digit())
    {
        format!("{name}:{error}")
    } else {
        format!("{name}: {error}")
    }
}

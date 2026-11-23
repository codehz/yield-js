// @restore
// @advance 1
// @fulfilled true

import { sleep, gc } from "test";

// Identity and synchronous dispatch assertions adapted from WPT dom/abort/event.any.js.
const a = new AbortController();
const b = new AbortController();
const dependent = AbortSignal.any([a.signal, b.signal]);
const nested = AbortSignal.any([a.signal, dependent, b.signal]);
const empty = AbortSignal.any([]);
const target = new EventTarget();
const reason = {};
const trace = [];
let valid = true;
function hit() { trace.push("hit"); }
target.addEventListener("x", hit, { signal: dependent });
target.removeEventListener("x", hit);
target.addEventListener("x", hit);
target.addEventListener("x", () => trace.push("removed"), { signal: nested });
a.signal.addEventListener("abort", event => {
  valid = dependent.aborted && nested.aborted && dependent.reason === reason &&
    nested.reason === reason && event.isTrusted && event.target === a.signal;
  trace.push("source");
  event.stopImmediatePropagation();
  gc();
});
a.signal.onabort = () => trace.push("stopped");
dependent.onabort = () => trace.push("dependent");
nested.onabort = () => trace.push("nested");

export async function run() {
  const signal = a.signal;
  if (signal.aborted || signal.reason !== undefined || empty.aborted) return false;
  await sleep(1);
  a.abort(reason);
  a.abort("ignored");
  b.abort("other");
  target.dispatchEvent(new Event("x"));
  if (!valid || signal !== a.signal || signal.reason !== reason ||
      trace.join(",") !== "source,dependent,nested,hit") return false;
  try { dependent.throwIfAborted(); }
  catch (error) { return error === reason; }
  return false;
}

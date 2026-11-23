// @pending ["wait:pipe-write"]
// @restore
// @fulfill ["wait:pipe-write", null]
// @fulfilled true

// Upstream: streams/piping/abort.any.js
import { wait } from "test";

export async function run() {
  const aborter = new AbortController();
  const reason = {};
  const calls = [];
  let writing;
  const began = new Promise(resolve => { writing = resolve; });
  const source = new ReadableStream({
    start(c) { c.enqueue(1); },
    cancel(value) { if (value !== reason) throw new Error("wrong cancellation reason"); calls.push("cancel"); }
  });
  const destination = new WritableStream({
    write(chunk) { calls.push("write" + chunk); writing(); return wait("pipe-write"); },
    abort(value) { if (value !== reason) throw new Error("wrong abort reason"); calls.push("abort"); }
  });
  const pipe = source.pipeTo(destination, { signal: aborter.signal });
  await began;
  aborter.abort(reason);
  if (calls.join(",") !== "write1" || !source.locked || !destination.locked) return false;
  let error;
  try { await pipe; } catch (value) { error = value; }
  return error === reason && !source.locked && !destination.locked
    && calls.includes("abort") && calls.includes("cancel") && calls.length === 3;
}

// @pending ["wait:inflight"]
// @restore
// @fulfill ["wait:inflight", null]
// @fulfilled true

// Upstream: streams/writable-streams/aborting.any.js
import { wait } from "test";

export async function run() {
  const reason = {};
  const calls = [];
  let signal;
  const stream = new WritableStream({
    start(controller) { signal = controller.signal; },
    write(chunk) { calls.push("write" + chunk); return wait("inflight"); },
    abort(value) { if (value !== reason) throw new Error("wrong reason"); calls.push("abort"); }
  });
  const writer = stream.getWriter();
  const closed = writer.closed.catch(error => error === reason);
  const first = writer.write(1);
  const second = writer.write(2).catch(error => error === reason);
  await Promise.resolve();
  signal.addEventListener("abort", () => calls.push("signal"));
  const abort = writer.abort(reason);
  const same = writer.abort({});
  if (!signal.aborted || signal.reason !== reason || abort !== same || calls.join(",") !== "write1,signal") return false;
  await first;
  await abort;
  return await second && await closed && calls.join(",") === "write1,signal,abort";
}

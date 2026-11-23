// @pending ["wait:write1"]
// @restore
// @fulfill ["wait:write1", true]
// @fulfilled true

// Upstream: streams/writable-streams/count-queuing-strategy.any.js
import { gc, wait } from "test";

export async function run() {
  const calls = [];
  const stream = new WritableStream({
    write(chunk) {
      calls.push("write" + chunk);
      if (chunk === 1) return wait("write1");
    },
    close() { calls.push("close"); }
  }, new CountQueuingStrategy({ highWaterMark: 1 }));
  const writer = stream.getWriter();
  const initialReady = writer.ready;
  const first = writer.write(1);
  const pressured = writer.ready;
  let released = false;
  pressured.then(() => { released = true; });
  const second = writer.write(2);
  await Promise.resolve();
  if (pressured === initialReady || released || writer.desiredSize !== -1) return false;
  const close = writer.close();
  // Queuing close releases backpressure immediately, even though the sink
  // close callback must remain behind both writes.
  await pressured;
  if (!released || calls.join(",") !== "write1") return false;
  gc();
  await first;
  await second;
  await close;
  await writer.closed;
  return calls.join(",") === "write1,write2,close" && writer.ready === pressured && stream.locked;
}

// @pending ["wait:t"]
// @restore
// @fulfill ["wait:t", 3]
// @fulfilled true

// Upstream: streams/transform-streams/backpressure.any.js
import { wait } from "test";

export async function run() {
  const seen = [];
  const transform = new TransformStream({
    async transform(chunk, controller) {
      seen.push(chunk);
      const value = await wait("t");
      controller.enqueue(value);
      seen.push(controller.desiredSize);
    },
    flush(controller) { controller.enqueue("end"); }
  });
  const writer = transform.writable.getWriter();
  const reader = transform.readable.getReader();
  const value = reader.read();
  await writer.write(1);
  await writer.close();
  const first = await value;
  return first.value === 3 && (await reader.read()).value === "end" && seen[0] === 1;
}

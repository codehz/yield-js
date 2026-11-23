// @pending ["wait:pull"]
// @restore
// @fulfill ["wait:pull", true]
// @fulfilled true

// Upstream: streams/readable-streams/count-queuing-strategy-integration.any.js
import { wait, gc } from "test";

export async function run() {
  let c;
  let pulls = 0;
  let sizes = 0;
  const chunk = {};
  const stream = new ReadableStream({
    start(controller) { c = controller; },
    async pull(controller) {
      pulls++;
      await wait("pull");
      gc();
      controller.enqueue(chunk);
      controller.close();
    }
  }, { highWaterMark: 0, size() { sizes++; throw new Error("unexpected size"); } });
  const reader = stream.getReader();
  const read = reader.read();
  const closed = reader.closed;
  const result = await read;
  await closed;
  return result.value === chunk && !result.done && (await reader.read()).done
    && reader.closed === closed && pulls === 1 && sizes === 0 && c.desiredSize === 0;
}

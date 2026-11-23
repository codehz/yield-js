// @fulfilled true

// Upstream: streams/readable-streams/count-queuing-strategy-integration.any.js
export async function run() {
  let controller;
  const stream = new ReadableStream({ start(c) { controller = c; } },
    new CountQueuingStrategy({ highWaterMark: 1 }));
  const reader = stream.getReader();
  controller.enqueue("a");
  controller.enqueue("b");
  if (controller.desiredSize !== -1 || !stream.locked) return false;
  if ((await reader.read()).value !== "a" || controller.desiredSize !== 0) return false;
  controller.close();
  if ((await reader.read()).value !== "b" || !(await reader.read()).done) return false;
  const closed = reader.closed;
  reader.releaseLock();
  if (stream.locked || reader.closed === closed) return false;
  let released = false;
  try { await reader.closed; } catch (e) { released = e instanceof TypeError; }
  return released && (await stream.getReader().read()).done;
}

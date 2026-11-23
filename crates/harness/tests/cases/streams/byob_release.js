// @fulfilled true

// Upstream behavior: streams/readable-byte-streams/general.any.js
export async function run() {
  let controller;
  const stream = new ReadableStream({ type: "bytes", start(c) { controller = c; } });
  const first = stream.getReader({ mode: "byob" });
  const read = first.read(new Uint16Array(2), { min: 2 });
  const request = controller.byobRequest;
  request.view[0] = 11;
  first.releaseLock();
  let rejected = false;
  try { await read; } catch (error) { rejected = error instanceof TypeError; }
  if (!rejected || stream.locked) return false;
  const second = stream.getReader();
  const next = second.read();
  request.respond(1);
  const result = await next;
  controller.close();
  return !result.done && result.value.length === 1 && result.value[0] === 11 && (await second.read()).done;
}

// @fulfilled true

// Upstream: streams/transform-streams/terminate.any.js
export async function run() {
  const flushError = {};
  const transform = new TransformStream({
    transform(chunk, controller) { controller.enqueue(chunk); },
    flush() { throw flushError; }
  });
  const writer = transform.writable.getWriter();
  const reader = transform.readable.getReader();
  const identity = {};
  const read = reader.read();
  await writer.write(identity);
  if ((await read).value !== identity) return false;
  const done = reader.read().catch(error => error === flushError);
  const closed = writer.close().catch(error => error === flushError);
  if (!(await done) || !(await closed)) return false;

  const terminating = new TransformStream({ transform(chunk, controller) { controller.terminate(); } });
  const end = terminating.readable.getReader().read();
  const w = terminating.writable.getWriter();
  await w.write(1);
  return (await end).done && await w.closed.catch(error => error instanceof TypeError);
}

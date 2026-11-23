// @fulfilled true

// Upstream: streams/writable-streams/count-queuing-strategy.any.js
export async function run() {
  const calls = [];
  let finish;
  const stream = new WritableStream({
    write(chunk) {
      calls.push(chunk);
      return new Promise(resolve => { finish = resolve; });
    }
  });
  const writer = new WritableStreamDefaultWriter(stream);
  const first = writer.write(1);
  const firstReady = writer.ready;
  const closed = writer.closed;
  const readied = firstReady.catch(error => error instanceof TypeError);
  const released = closed.catch(error => error instanceof TypeError);
  await Promise.resolve();
  writer.releaseLock();
  if (stream.locked || !(await readied) || !(await released)) return false;
  const secondWriter = stream.getWriter();
  const ready = secondWriter.ready;
  let secondReady = false;
  ready.then(() => { secondReady = true; });
  await Promise.resolve();
  if (secondReady || calls.join(",") !== "1") return false;
  finish();
  await first;
  await ready;
  return secondReady && secondWriter.desiredSize === 1 && secondWriter.ready === ready;
}

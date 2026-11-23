// @fulfilled true

// Upstream: streams/readable-streams/general.any.js and streams/interfaces/ReadableStreamDefaultReader/releaseLock.any.js
export async function run() {
  const reason = {};
  let controller;
  let canceled;
  const source = {
    start(c) { if (this !== source) throw reason; controller = c; },
    cancel(value) { canceled = value; }
  };
  const stream = new ReadableStream(source, { highWaterMark: 0 });
  const first = stream.getReader();
  const pending = first.read();
  const closed = first.closed;
  first.releaseLock();
  let readError;
  let closedError;
  try { await pending; } catch (error) { readError = error; }
  try { await closed; } catch (error) { closedError = error; }
  if (!(readError instanceof TypeError) || !(closedError instanceof TypeError) || canceled !== undefined) return false;
  const second = new ReadableStreamDefaultReader(stream);
  const read = second.read();
  controller.enqueue(reason);
  if ((await read).value !== reason) return false;
  await second.cancel(reason);
  return canceled === reason && (await second.read()).done && stream.locked;
}

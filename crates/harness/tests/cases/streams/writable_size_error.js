// @fulfilled true

// Upstream: streams/writable-streams/reentrant-strategy.any.js
export async function run() {
  const firstReason = {};
  const secondReason = {};
  let controller;
  let writeCount = 0;
  let writer;
  const stream = new WritableStream({
    start(c) { controller = c; },
    write() { writeCount++; }
  }, {
    size() {
      controller.error(firstReason);
      throw secondReason;
    }
  });
  writer = stream.getWriter();
  await Promise.resolve();
  const closed = writer.closed.catch(reason => reason === firstReason);
  let result;
  try { await writer.write(1); } catch (reason) { result = reason; }
  return result === firstReason && await closed && writeCount === 0 && writer.desiredSize === null;
}

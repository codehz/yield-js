// @fulfilled true

// Upstream: streams/readable-streams/tee.any.js
export async function run() {
  let controller;
  const reasonA = {};
  const reasonB = {};
  const source = new ReadableStream({
    start(c) { controller = c; },
    cancel(reason) {
      if (!Array.isArray(reason) || reason[0] !== reasonA || reason[1] !== reasonB) throw new Error("bad composite");
    }
  });
  const [left, right] = source.tee();
  const leftReader = left.getReader();
  const rightReader = right.getReader();
  const chunk = {};
  const first = leftReader.read();
  controller.enqueue(chunk);
  const result = await first;
  const mirrored = await rightReader.read();
  if (result.value !== chunk || mirrored.value !== chunk) return false;
  await Promise.all([leftReader.cancel(reasonA), rightReader.cancel(reasonB)]);
  return true;
}

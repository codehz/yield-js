// @fulfilled true
// Upstream: streams/readable-byte-streams/tee.any.js

export async function run() {
  const source = new ReadableStream({
    type: "bytes",
    pull(controller) {
      controller.enqueue(new Uint8Array([1, 2, 3, 4, 5]));
      controller.close();
    },
  });

  const [left, right] = source.tee();
  const [leftReader, rightReader] = [left.getReader({ mode: "byob" }), right.getReader()];
  const view = new Uint8Array(3);
  const first = await leftReader.read(view);
  if (view.buffer.detached !== true) throw new Error("BYOB buffer was not transferred");
  if (first.value[0] !== 1 || first.value[2] !== 3) throw new Error("BYOB bytes");
  first.value[0] = 99;
  const mirrored = await rightReader.read();
  if (mirrored.value.buffer === first.value.buffer) throw new Error("shared buffer");
  if (mirrored.value.length !== 3 || mirrored.value[0] !== 1) throw new Error("mirror");
  const remainder = await rightReader.read();
  if (remainder.value.length !== 2 || remainder.value[0] !== 4) throw new Error("remainder");
  const rest = await leftReader.read(new Uint8Array(3));
  if (rest.value.length !== 2 || rest.value[0] !== 4) throw new Error("left rest");
  if (!(await rightReader.read()).done) throw new Error("right not done");
  if (!(await leftReader.read(new Uint8Array(1))).done) throw new Error("left not done");
  return true;
}

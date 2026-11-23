// @fulfilled true

// Upstream: streams/readable-byte-streams/read-min.any.js
export async function run() {
  let controller;
  const stream = new ReadableStream({ type: "bytes", start(c) { controller = c; } });
  const reader = stream.getReader({ mode: "byob" });
  const original = new Uint16Array(1);
  let rangeError;
  let zeroError;
  try { await reader.read(original, { min: 2 }); } catch (error) { rangeError = error; }
  try { await reader.read(original, { min: 0 }); } catch (error) { zeroError = error; }
  if (!(rangeError instanceof RangeError) || !(zeroError instanceof TypeError) || original.buffer.detached) return false;

  const supplied = new Uint8Array([1, 2, 3, 4]);
  controller.enqueue(supplied.subarray(1));
  if (!supplied.buffer.detached) return false;
  const pair = await reader.read(original);
  if (pair.done || new Uint8Array(pair.value.buffer).join(",") !== "2,3") return false;
  const last = await reader.read(new Uint8Array(2));
  controller.close();
  return !last.done && last.value.join(",") === "4";
}

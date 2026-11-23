// @pending ["wait:cut"]
// @restore
// @fulfill ["wait:cut", null]
// @fulfilled true

// Upstream: compression/compression-stream.any.js
import { wait } from "test";

const gzip = new Uint8Array([31,139,8,0,0,0,0,0,2,255,203,72,205,201,201,87,200,64,144,0,128,136,249,229,17,0,0,0]);

export async function run() {
  const decoder = new DecompressionStream("gzip");
  const writer = decoder.writable.getWriter();
  const reader = decoder.readable.getReader();
  const text = (async () => {
    let out = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return out;
      out += new TextDecoder().decode(value, { stream: true });
    }
  })();
  await writer.write(gzip.subarray(0, 10));
  await wait("cut");
  await writer.write(gzip.subarray(10));
  await writer.close();
  if (await text !== "hello hello hello") return false;

  const encoder = new CompressionStream("gzip");
  const ew = encoder.writable.getWriter();
  const er = encoder.readable.getReader();
  const bytes = (async () => {
    const parts = [];
    for (;;) {
      const { value, done } = await er.read();
      if (done) return parts;
      parts.push(...value);
    }
  })();
  ew.write(new Uint8Array([1, 2, 3]));
  await ew.close();
  const compressed = Uint8Array.from(await bytes);
  const again = new DecompressionStream("gzip");
  const aw = again.writable.getWriter();
  const ar = again.readable.getReader();
  const decoded = ar.read().then(r => r.value);
  aw.write(compressed);
  await aw.close();
  return (await decoded).join(",") === "1,2,3";
}

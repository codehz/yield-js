// @fulfilled true

// Upstream: compression/decompression-stream.any.js
const payload = "hello hello hello";
const gzip = new Uint8Array([31,139,8,0,0,0,0,0,2,255,203,72,205,201,201,87,200,64,144,0,128,136,249,229,17,0,0,0]);
const zlibBytes = new Uint8Array([120,156,203,72,205,201,201,87,200,64,144,0,58,46,6,125]);
const raw = new Uint8Array([203,72,205,201,201,87,200,64,144,0]);
const empty = new Uint8Array([31,139,8,0,0,0,0,0,2,255,3,0,0,0,0,0,0,0,0,0]);

async function inflate(format, bytes, split) {
  const stream = new DecompressionStream(format);
  const writer = stream.writable.getWriter();
  const reader = stream.readable.getReader();
  const text = (async () => {
    let out = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return out;
      out += new TextDecoder().decode(value, { stream: true });
    }
  })();
  for (let i = 0; i < bytes.length; i += split) writer.write(bytes.subarray(i, i + split));
  await writer.close();
  return text;
}

async function roundTrip(format) {
  const stream = new CompressionStream(format);
  const writer = stream.writable.getWriter();
  const reader = stream.readable.getReader();
  const bytes = (async () => {
    const parts = [];
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return parts;
      parts.push(...value);
    }
  })();
  writer.write(new TextEncoder().encode("ab"));
  writer.write(new TextEncoder().encode("cd"));
  await writer.close();
  return Uint8Array.from(await bytes);
}

export async function run() {
  if (await inflate("gzip", gzip, 3) !== payload) return false;
  if (await inflate("deflate", zlibBytes, 2) !== payload) return false;
  if (await inflate("deflate-raw", raw, 1) !== payload) return false;
  if (await inflate("gzip", empty, 5) !== "") return false;
  for (const format of ["gzip", "deflate", "deflate-raw", "brotli"]) {
    const compressed = await roundTrip(format);
    if (compressed.length === 0) return false;
    const stream = new DecompressionStream(format);
    const writer = stream.writable.getWriter();
    const reader = stream.readable.getReader();
    const done = (async () => {
      let text = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) return text;
        text += new TextDecoder().decode(value);
      }
    })();
    writer.write(compressed.subarray(0, 1));
    writer.write(compressed.subarray(1));
    await writer.close();
    if (await done !== "abcd") return false;
  }
  let invalid;
  try { new CompressionStream("zip"); } catch (error) { invalid = error; }
  let corrupt;
  const bad = new DecompressionStream("deflate-raw");
  const badWriter = bad.writable.getWriter();
  const badRead = bad.readable.getReader().read();
  try { await badWriter.write(new Uint8Array([0xff, 0xff, 0xff])); }
  catch (error) { corrupt = error; }
  try { await badWriter.close(); } catch (error) { corrupt = corrupt || error; }
  let readError;
  try { await badRead; } catch (error) { readError = error; }
  return invalid instanceof TypeError && corrupt instanceof TypeError && readError instanceof TypeError;
}

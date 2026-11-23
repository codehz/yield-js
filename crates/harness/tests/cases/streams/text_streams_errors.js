// @fulfilled true

// Upstream: encoding/textdecoder-stream.any.js and encoding/textencoder-stream.any.js
export async function run() {
  const decoder = new TextDecoderStream("utf-16le", { fatal: true });
  const writer = decoder.writable.getWriter();
  const reader = decoder.readable.getReader();
  const first = reader.read();
  writer.write(new Uint8Array([0x41]));
  await Promise.resolve();
  writer.write(new Uint8Array([0x00, 0x42, 0x00]));
  writer.close();
  const text = await first;
  if (text.value !== "AB" || !(await reader.read()).done) return false;

  const fatal = new TextDecoderStream("utf-8", { fatal: true });
  const fw = fatal.writable.getWriter();
  const fr = fatal.readable.getReader();
  const read = fr.read();
  await fw.write(new Uint8Array([0xe2, 0x82]));
  let writeError;
  try { await fw.write(new Uint8Array([0x28])); } catch (value) { writeError = value; }
  let error;
  try { await read; } catch (value) { error = value; }
  let laterError;
  try { await fw.write(new Uint8Array([0x41])); } catch (value) { laterError = value; }
  return error instanceof TypeError && writeError instanceof TypeError && laterError instanceof TypeError;
}

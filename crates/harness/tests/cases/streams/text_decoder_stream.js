// @fulfilled true

// Upstream: encoding/textdecoder-stream.any.js
export async function run() {
  const decoder = new TextDecoderStream("utf-8", { ignoreBOM: false });
  const writer = decoder.writable.getWriter();
  const reader = decoder.readable.getReader();
  const first = reader.read();
  writer.write(new Uint8Array([0xef, 0xbb, 0xbf, 0xf0, 0x9f]));
  await Promise.resolve();
  writer.write(new Uint8Array([0x98, 0x80, 0x21]));
  await writer.close();
  const text = await first;
  return text.value === "\u{1F600}!" && (await reader.read()).done;
}

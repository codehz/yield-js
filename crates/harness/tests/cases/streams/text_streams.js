// @pending ["wait:split"]
// @restore
// @fulfill ["wait:split", null]
// @fulfilled true

// Upstream: encoding/textencoder-stream.any.js and encoding/textdecoder-stream.any.js
import { wait } from "test";

export async function run() {
  const encoder = new TextEncoderStream();
  const writer = encoder.writable.getWriter();
  const reader = encoder.readable.getReader();
  const encoded = reader.read();
  writer.write("a\uD83D");
  writer.write("\uDE00b");
  writer.close();
  const firstChunk = await encoded;
  const secondChunk = await reader.read();
  const end = await reader.read();
  if (new TextDecoder().decode(firstChunk.value) !== "a" || new TextDecoder().decode(secondChunk.value) !== "\u{1F600}b" || !end.done) return false;

  const decoder = new TextDecoderStream("utf-8", { ignoreBOM: false });
  const dw = decoder.writable.getWriter();
  const dr = decoder.readable.getReader();
  const first = dr.read();
  const bytes = new Uint8Array([0xef, 0xbb, 0xbf, 0xf0, 0x9f]);
  dw.write(bytes);
  await wait("split");
  dw.write(new Uint8Array([0x98, 0x80, 0x21]));
  dw.close();
  const text = await first;
  return text.value === "\u{1F600}!" && (await dr.read()).done;
}

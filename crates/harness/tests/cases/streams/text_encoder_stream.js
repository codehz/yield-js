// @fulfilled true

// Upstream: encoding/textencoder-stream.any.js
export async function run() {
  const encoder = new TextEncoderStream();
  const writer = encoder.writable.getWriter();
  const reader = encoder.readable.getReader();
  const a = reader.read();
  writer.write("a\uD83D");
  writer.write("\uDE00b");
  writer.close();
  const first = await a;
  const second = await reader.read();
  const end = await reader.read();
  return new TextDecoder().decode(first.value) === "a" && new TextDecoder().decode(second.value) === "\u{1F600}b" && end.done;
}

// @fulfilled true
// Upstream: compression/compression-stream.any.js

const HEX = "1fd704f81d09368ef26d5c65e21e1ea2c699d5ce3901a2c2319f593eea1613ab0975e05e667417978aadc42c1d898554da58e7434cb9d4d6c75cfbdcf781723a7b0f";
const EXPECTED = "brotli reference vector: abcdefghijklmnopqrstuvwxyz0123456789\n".repeat(20);

export async function run() {
  const compressed = Uint8Array.from(HEX.match(/../g).map((pair) => parseInt(pair, 16)));
  const stream = new DecompressionStream("brotli");
  const writer = stream.writable.getWriter();
  const reader = stream.readable.getReader();
  const decode = new TextDecoder();
  let output = "";
  const drain = (async () => {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      output += decode.decode(value, { stream: true });
    }
  })();
  // Split every reference block boundary so the decoder resumes from its state.
  for (let offset = 0; offset < compressed.length;) {
    const end = Math.min(compressed.length, offset + 7);
    await writer.write(compressed.subarray(offset, end));
    offset = end;
  }
  await writer.close();
  await drain;
  output += decode.decode();
  return output === EXPECTED;
}

// @fulfilled true

// Upstream: streams/writable-streams/reentrant-strategy.any.js
export async function run() {
  let writer;
  const calls = [];
  const stream = new WritableStream({
    write(chunk) { calls.push("write" + chunk); }
  }, {
    size(chunk) {
      calls.push("size" + chunk);
      if (chunk > 0) {
        writer.write(chunk - 1).then(() => calls.push("done" + (chunk - 1)));
      }
      return chunk;
    }
  });
  writer = stream.getWriter();
  await writer.write(2);
  return calls.join(",") === "size2,size1,size0,write0,write1,done0,write2,done1";
}

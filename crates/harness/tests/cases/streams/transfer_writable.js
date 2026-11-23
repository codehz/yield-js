// @pending ["wait:write"]
// @restore
// @fulfill ["wait:write", null]
// @fulfilled true

// Upstream: streams/transferring/writable-streams/transfer.any.js
import { wait } from "test";

export async function run() {
  const writes = [];
  const sink = new WritableStream({
    write(chunk) { writes.push(chunk); return wait("write"); },
    close() { writes.push("close"); }
  });
  const [transferred] = structuredClone([sink], { transfer: [sink] });
  if (!sink.locked || transferred.locked) return false;
  const writer = transferred.getWriter();
  const write = writer.write(7);
  await writer.close();
  await write;
  return writes.join(",") === "7,close";
}

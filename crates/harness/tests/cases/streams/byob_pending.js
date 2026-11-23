// @pending ["wait:partial"]
// @restore
// @fulfill ["wait:partial", true]
// @fulfilled true

// Upstream: streams/readable-byte-streams/read-min.any.js
import { wait, gc } from "test";

export async function run() {
  let pulls = 0;
  let stale;
  let staleView;
  const stream = new ReadableStream({
    type: "bytes",
    async pull(controller) {
      pulls++;
      const request = controller.byobRequest;
      if (request !== controller.byobRequest) throw new Error("unstable request identity");
      if (pulls === 1) {
        stale = request;
        staleView = request.view;
        if (staleView.byteOffset !== 2) throw new Error("wrong initial offset");
        staleView[0] = 7;
        request.respond(1);
        await wait("partial");
        gc();
      } else {
        if (request.view.byteOffset !== 3 || request === stale) throw new Error("wrong remaining range");
        request.view.set([8, 9, 10]);
        request.respond(3);
        controller.close();
      }
    }
  });
  const input = new Uint16Array(new ArrayBuffer(10), 2, 4);
  const reader = new ReadableStreamBYOBReader(stream);
  const pending = reader.read(input, { min: 2 });
  if (!input.buffer.detached) return false;
  const { value, done } = await pending;
  if (done || !(value instanceof Uint16Array) || value.byteOffset !== 2 || value.length !== 2) return false;
  const bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  if (bytes.join(",") !== "7,8,9,10" || stale.view !== null || !staleView.buffer.detached || pulls !== 2) return false;
  try { stale.respond(1); } catch (e) { return e instanceof TypeError; }
  return false;
}

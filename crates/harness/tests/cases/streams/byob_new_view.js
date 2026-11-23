// @fulfilled true

// Upstream: streams/readable-byte-streams/general.any.js
export async function run() {
  let request;
  let oldView;
  const stream = new ReadableStream({
    type: "bytes",
    pull(controller) {
      request = controller.byobRequest;
      oldView = request.view;
      const buffer = new ArrayBuffer(oldView.buffer.byteLength);
      const view = new Uint8Array(buffer, oldView.byteOffset, 2);
      view.set([12, 13]);
      request.respondWithNewView(view);
      if (!buffer.detached) throw new Error("replacement buffer was not detached");
      controller.close();
    }
  });
  const reader = stream.getReader({ mode: "byob" });
  const result = await reader.read(new DataView(new ArrayBuffer(8), 2, 4), { min: 2 });
  return !result.done && result.value instanceof DataView && result.value.byteOffset === 2
    && result.value.byteLength === 2 && result.value.getUint8(0) === 12 && result.value.getUint8(1) === 13
    && request.view === null;
}

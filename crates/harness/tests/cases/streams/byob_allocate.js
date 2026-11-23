// @fulfilled true

// Upstream: streams/readable-byte-streams/general.any.js
export async function run() {
  const stream = new ReadableStream({
    type: "bytes",
    autoAllocateChunkSize: 16,
    pull(c) {
      const request = c.byobRequest;
      if (!(request.view instanceof Uint8Array) || request.view.byteLength !== 16) throw new Error("wrong allocation");
      request.view.set([4, 9]);
      request.respond(2);
      c.close();
    }
  });
  const reader = stream.getReader();
  const first = await reader.read();
  if (first.done || first.value.join(",") !== "4,9" || !(await reader.read()).done) return false;

  let closeError;
  const partial = new ReadableStream({
    type: "bytes",
    pull(c) {
      c.byobRequest.respond(1);
      try { c.close(); } catch (error) { closeError = error; }
    }
  });
  try { await partial.getReader({ mode: "byob" }).read(new Uint16Array(2)); }
  catch (error) { return error === closeError && error instanceof TypeError; }
  return false;
}

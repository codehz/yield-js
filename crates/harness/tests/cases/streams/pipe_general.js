// @fulfilled true

// Upstream: streams/piping/general.any.js
export async function run() {
  const chunks = [];
  const source = new ReadableStream({
    start(c) { c.enqueue(1); c.enqueue(2); c.close(); }
  });
  const destination = new WritableStream({
    write(chunk) { chunks.push(chunk); },
    close() { chunks.push("close"); }
  });
  await source.pipeTo(destination);
  if (chunks.join(",") !== "1,2,close") return false;
  const reason = {};
  const endless = new ReadableStream({ start(c) { c.enqueue(1); } });
  const failing = new WritableStream({ write() { throw reason; } });
  let error;
  try { await endless.pipeTo(failing); } catch (value) { error = value; }
  return error === reason && !endless.locked && !failing.locked;
}

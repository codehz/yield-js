// @fulfilled true

// Upstream: streams/transferring/transform-streams/transfer.any.js
export async function run() {
  const transform = new TransformStream({ transform(chunk, controller) { controller.enqueue(chunk * 2); } });
  const [proxy] = structuredClone([transform], { transfer: [transform] });
  if (!transform.readable.locked || !transform.writable.locked || proxy.readable.locked || proxy.writable.locked) return false;
  const writer = proxy.writable.getWriter();
  const reader = proxy.readable.getReader();
  const read = reader.read();
  await writer.write(4);
  const result = await read;
  if (result.value !== 8) return false;
  await writer.close();
  if (!(await reader.read()).done) return false;

  const locked = new ReadableStream();
  locked.getReader();
  let error;
  try { structuredClone([locked], { transfer: [locked] }); } catch (value) { error = value; }
  return error && error.name === "DataCloneError";
}

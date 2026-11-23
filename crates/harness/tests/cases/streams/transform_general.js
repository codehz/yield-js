// @fulfilled true

// Upstream: streams/transform-streams/general.any.js
export async function run() {
  const seen = [];
  const transform = new TransformStream({
    start(controller) { seen.push("start"); },
    transform(chunk, controller) { seen.push(chunk); if (chunk === 1) controller.enqueue(chunk * 10); },
    flush(controller) { seen.push("flush"); controller.enqueue("end"); }
  });
  const writer = transform.writable.getWriter();
  const reader = transform.readable.getReader();
  const a = reader.read();
  const b = reader.read();
  const c = reader.read();
  await writer.write(1);
  await writer.write(2);
  await writer.close();
  const values = [(await a).value, (await b).value, (await c).value];
  return values.join(",") === "10,end," && seen.join(",") === "start,1,2,flush"
    && (await reader.read()).done && transform.readable !== transform.writable;
}

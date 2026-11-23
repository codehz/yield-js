// @pending ["wait:chunk"]
// @restore
// @fulfill ["wait:chunk", 9]
// @fulfilled true

// Upstream: streams/readable-streams/tee.any.js
import { wait } from "test";

export async function run() {
  const source = new ReadableStream({
    async pull(controller) {
      controller.enqueue(await wait("chunk"));
      controller.close();
    }
  });
  const [left, right] = source.tee();
  const a = left.getReader().read();
  const b = right.getReader().read();
  const first = await a;
  const second = await b;
  return first.value === 9 && second.value === 9 && first.value === second.value;
}

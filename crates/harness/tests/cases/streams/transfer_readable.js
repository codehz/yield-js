// @pending ["wait:chunk"]
// @restore
// @fulfill ["wait:chunk", 5]
// @fulfilled true

// Upstream: streams/transferring/readable-streams/transfer.any.js
import { wait } from "test";

export async function run() {
  const chunk = {};
  const source = new ReadableStream({ async pull(c) { c.enqueue(await wait("chunk")); c.close(); } });
  const [transferred] = structuredClone([source], { transfer: [source] });
  if (!source.locked || transferred.locked) return false;
  let cloneError;
  try { structuredClone(source); } catch (error) { cloneError = error.name; }
  if (cloneError !== "DataCloneError") return false;
  const reader = transferred.getReader();
  const result = await reader.read();
  return result.value === 5 && !result.done && (await reader.read()).done;
}

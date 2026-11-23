// @advance 4
// @restore
// @advance 5
// @advance 1
// @fulfilled true

import { sleep } from "test";

// Zero-delay and TimeoutError assertions adapted from WPT dom/abort/timeout.any.js.
export async function run() {
  const zero = AbortSignal.timeout(0);
  const later = AbortSignal.timeout(10);
  const log = [];
  zero.onabort = () => log.push("zero");
  later.onabort = e => log.push(e.isTrusted ? "timeout" : "untrusted");
  if (zero.aborted || later.aborted) return false;
  await sleep(9);
  if (!zero.aborted || later.aborted || log.join(",") !== "zero") return false;
  return await new Promise(resolve => later.addEventListener("abort", () => {
    resolve(later.reason instanceof DOMException && later.reason.name === "TimeoutError" &&
      later.reason.code === 23 && log.join(",") === "zero,timeout");
  }));
}

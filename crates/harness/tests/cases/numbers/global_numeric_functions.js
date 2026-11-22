// @pending ["wait:global-numbers"]
// @restore
// @fulfill ["wait:global-numbers", null]
// @fulfilled true

import { expect, wait } from "test";

export async function run() {
  const int = parseInt;
  const float = parseFloat;
  await wait("global-numbers");
  expect(int === Number.parseInt && int === globalThis.parseInt).toBe(true);
  expect(float === Number.parseFloat && float === globalThis.parseFloat).toBe(true);
  expect(int(" -0xFEtail")).toBe(-254);
  expect(Object.is(float("-0tail"), -0)).toBe(true);
  let order = "";
  expect(int({toString() { order += "s"; return "101suffix"; }},
             {valueOf() { order += "n"; return 2; }})).toBe(5);
  expect(order).toBe("sn");
  expect(isFinite("2") && !Number.isFinite("2") && !isFinite(Infinity)).toBe(true);
  expect(isNaN() && isNaN("junk") && !Number.isNaN("junk") && !isNaN(null)).toBe(true);
  for (const fn of [isNaN, isFinite]) {
    const marker = {};
    try { fn({valueOf() { throw marker; }}); return "lost conversion error"; }
    catch (error) { expect(error === marker).toBe(true); }
    for (const arg of [1n, Symbol()]) {
      try { fn(arg); return "accepted non-Number primitive"; }
      catch (error) { expect(error instanceof TypeError).toBe(true); }
    }
  }
  return true;
}

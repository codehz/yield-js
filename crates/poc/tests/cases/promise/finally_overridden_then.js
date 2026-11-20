// @pending ["wait:cleanup"]
// @restore
// @fulfill ["wait:cleanup", 8]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const p = Promise.resolve(1);
  const marker = {};
  let result;
  let argsOK = false;
  p.then = function (ok, bad) {
    argsOK = this === p && typeof ok === "function" && typeof bad === "function";
    result = ok(42);
    return marker;
  };
  let cleaned = false;
  const returned = p.finally(async function () {
    await wait("cleanup");
    cleaned = true;
  });
  const value = await result;
  p.then = (ok, bad) => ok === 7 && bad === 7;
  let reason;
  try { await Promise.reject("original").finally(() => Promise.resolve(0)); }
  catch (error) { reason = error; }
  expect(argsOK).toBe(true);
  expect(returned).toBe(marker);
  expect(cleaned).toBe(true);
  expect(value).toBe(42);
  expect(p.finally(7)).toBe(true);
  expect(await Promise.resolve(9).finally(null)).toBe(9);
  expect(reason).toBe("original");
}

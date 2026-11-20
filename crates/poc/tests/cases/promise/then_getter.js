// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let reads = 0;
  const thenable = {
    n: 7,
    accept(resolve) { resolve(this.n); },
    get then() { reads += 1; return this.accept; }
  };
  expect(await thenable).toBe(7);
  expect(reads).toBe(1);
  expect(await Promise.resolve(thenable)).toBe(7);
  expect(reads).toBe(2);
  expect(await Promise.resolve(5).finally(() => thenable)).toBe(5);
  expect(reads).toBe(3);

  let noncallableReads = 0;
  const noncallable = {get then() { noncallableReads += 1; return 0; }};
  expect(await Promise.resolve(noncallable)).toBe(noncallable);
  expect(noncallableReads).toBe(1);
  expect(await Promise.resolve(5).finally(() => noncallable)).toBe(5);
  expect(noncallableReads).toBe(2);

  let throws = 0;
  const bad = {get then() { throws += 1; throw "bad"; }};
  let awaitError;
  try { await bad; }
  catch (error) { awaitError = error; }
  expect(awaitError).toBe("bad");
  expect(throws).toBe(1);
  const rejected = Promise.resolve(bad);
  let resolveError;
  try { await rejected; }
  catch (error) { resolveError = error; }
  expect(resolveError).toBe("bad");
  expect(throws).toBe(2);
  let fulfilledCleanupError;
  try { await Promise.resolve(5).finally(() => bad); }
  catch (error) { fulfilledCleanupError = error; }
  expect(fulfilledCleanupError).toBe("bad");
  expect(throws).toBe(3);
  let rejectedCleanupError;
  try { await Promise.reject("original").finally(() => bad); }
  catch (error) { rejectedCleanupError = error; }
  expect(rejectedCleanupError).toBe("bad");
  expect(throws).toBe(4);
}

// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const resolve = Promise.resolve;
  let reads = 0;
  let calls = "";
  let correct = true;
  Object.defineProperty(Promise, "resolve", {
    configurable: true,
    get() {
      reads++;
      return function (value) {
        correct = correct && this === Promise && arguments.length === 1;
        calls += value;
        return Reflect.apply(resolve, this, [value + 10]);
      };
    },
  });
  const all = await Promise.all([1, 2]);
  const settled = await Promise.allSettled([3]);
  const raced = await Promise.race([4]);
  const any = await Promise.any([5]);
  Object.defineProperty(Promise, "resolve", { configurable: true, value: 0 });
  const bad = Promise.all([]);
  Object.defineProperty(Promise, "resolve", { configurable: true, get() { throw "get"; } });
  const getter = Promise.race([]);
  Object.defineProperty(Promise, "resolve", { value() { throw "call"; } });
  const invoked = Promise.any([1]);
  let errors = "";
  for (const promise of [bad, getter, invoked]) {
    try { await promise; }
    catch (error) { errors += typeof error === "string" ? error : error.name; }
  }
  expect(correct).toBe(true);
  expect(reads).toBe(4);
  expect(calls).toBe("12345");
  expect(all[1]).toBe(12);
  expect(settled[0].value).toBe(13);
  expect(raced).toBe(14);
  expect(any).toBe(15);
  expect(errors).toBe("TypeErrorgetcall");
}

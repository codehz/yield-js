// @returns undefined

import { expect } from 'test';

export function run() {
  function C() {}
  const instance = new C();
  const child = Object.create(Object.create(C.prototype));
  const bound = C.bind(null);
  bound.prototype = {};
  expect(instance instanceof C).toBe(true);
  expect(child instanceof C).toBe(true);
  expect(instance instanceof bound).toBe(true);
  expect(C.prototype instanceof C).toBe(false);
  expect({ prototype: C.prototype } instanceof C).toBe(false);
  expect(42 instanceof C).toBe(false);
  expect({} instanceof Object).toBe(true);
  expect([] instanceof Object).toBe(true);
  expect(new Error() instanceof Error).toBe(true);
  expect(new TypeError() instanceof Error).toBe(true);
  expect(Promise.resolve(1) instanceof Promise).toBe(true);
}

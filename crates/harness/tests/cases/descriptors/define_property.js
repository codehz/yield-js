// @pending ["wait:define"]
// @restore
// @fulfill ["wait:define", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

function Descriptor() {}

export async function run() {
  const define = Object.defineProperty;
  const describe = Object.getOwnPropertyDescriptor;
  const symbol = Symbol("state");
  const target = { a: 1, b: 2 };
  let state = 3;
  let trace = "";
  const getter = function () { return this.a + state; };
  const setter = function (n) { state = n; };
  Descriptor.prototype = {
    get enumerable() { trace += "e"; this.configurable = 1; return 1; },
    get get() { trace += "g"; return getter; },
  };
  const descriptor = new Descriptor();
  descriptor.set = setter;
  expect(define(target, "a", descriptor)).toBe(target);
  expect(trace).toBe("eg");
  expect(describe(target, "a").get).toBe(getter);
  expect(describe(target, "a").set).toBe(setter);
  define(target, "a", { get: undefined });
  target.a = 8;
  define(target, "a", { get: function () { return this.b + state; } });
  define(target, symbol, { value: 3 });
  define(target, "empty", { get: undefined });
  define(target, "implicit", {});
  const bound = setter.bind(null, 12);
  define(bound, "x", { get: Object.keys });
  await wait("define");
  expect(define).toBe(Object.defineProperty);
  expect(target.a).toBe(10);
  expect(target[symbol]).toBe(3);
  const keys = Object.keys(target);
  expect(keys.length).toBe(2);
  expect(keys[0]).toBe("a");
  expect(keys[1]).toBe("b");
  const fixed = describe(target, symbol);
  expect(fixed.enumerable).toBe(false);
  expect(fixed.configurable).toBe(false);
  expect(fixed.writable).toBe(false);
  const empty = describe(target, "empty");
  expect("get" in empty).toBe(true);
  expect(empty.get).toBe(undefined);
  expect("value" in empty).toBe(false);
  expect("empty" in target).toBe(true);
  const implicit = describe(target, "implicit");
  expect("value" in implicit).toBe(true);
  expect(implicit.value).toBe(undefined);
  expect(implicit.writable).toBe(false);
  expect(describe(bound, "x").get).toBe(Object.keys);
  define(target, "a", { value: 4 });
  const changed = describe(target, "a");
  expect(changed.value).toBe(4);
  expect(changed.enumerable).toBe(true);
  expect(changed.configurable).toBe(true);
  expect(changed.writable).toBe(false);
}

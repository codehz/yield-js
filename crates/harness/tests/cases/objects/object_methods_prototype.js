// @pending ["wait:object-methods"]
// @restore
// @fulfill ["wait:object-methods", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

function expectThrow(action, check) {
  let thrown;
  try { action(); } catch (error) { thrown = error; }
  check(thrown);
}

function typeError(error) {
  expect(error?.name).toBe("TypeError");
}

export async function run() {
  const { isPrototypeOf, propertyIsEnumerable, toLocaleString } = Object.prototype;
  await wait("object-methods");
  expect(isPrototypeOf).toBe(Object.prototype.isPrototypeOf);
  expect(propertyIsEnumerable).toBe(Object.prototype.propertyIsEnumerable);
  expect(toLocaleString).toBe(Object.prototype.toLocaleString);
  expect(isPrototypeOf.length).toBe(1);
  expect(propertyIsEnumerable.length).toBe(1);
  expect(toLocaleString.length).toBe(0);

  const parent = { inherited: 1 };
  const child = Object.create(parent);
  const grandchild = Object.create(child);
  expect(isPrototypeOf.call(parent, grandchild)).toBe(true);
  expect(isPrototypeOf.call(parent, parent)).toBe(false);
  expect(isPrototypeOf.call(Object.prototype, Object.create(null))).toBe(false);
  expect(isPrototypeOf.call(null, 1)).toBe(false);
  expectThrow(() => isPrototypeOf.call(null, {}), typeError);
  const proxyParent = new Proxy(parent, {});
  const proxyChild = Object.create(proxyParent);
  expect(isPrototypeOf.call(proxyParent, proxyChild)).toBe(true);
  expect(isPrototypeOf.call(parent, proxyChild)).toBe(false);

  const symbol = Symbol("own");
  Object.defineProperty(child, symbol, {
    get() { throw "enumerability read the getter"; }, enumerable: true,
  });
  Object.defineProperty(child, "hidden", { value: 2 });
  expect(propertyIsEnumerable.call(child, symbol)).toBe(true);
  expect(propertyIsEnumerable.call(child, "hidden")).toBe(false);
  expect(propertyIsEnumerable.call(child, "inherited")).toBe(false);
  expect(propertyIsEnumerable.call("😀", 1)).toBe(true);
  expect(propertyIsEnumerable.call("😀", "01")).toBe(false);
  expect(propertyIsEnumerable.call("😀", 2)).toBe(false);
  expect(propertyIsEnumerable.call("😀", "length")).toBe(false);
  expect(propertyIsEnumerable.call({ undefined: 1 })).toBe(true);

  const log = [];
  const proxy = new Proxy(child, {
    getOwnPropertyDescriptor(target, key) {
      log.push(key);
      return Reflect.getOwnPropertyDescriptor(target, key);
    },
    get() { throw "unexpected get"; },
    ownKeys() { throw "unexpected ownKeys"; },
    has() { throw "unexpected has"; },
  });
  expect(propertyIsEnumerable.call(proxy, symbol)).toBe(true);
  expect(log).toEqual([symbol]);
  const key = { toString() { throw "bad key"; } };
  expectThrow(() => propertyIsEnumerable.call(null, key), error => expect(error).toBe("bad key"));
  expectThrow(() => propertyIsEnumerable.call(null, "x"), typeError);

  const result = {};
  const receiver = {
    get toString() {
      expect(this).toBe(receiver);
      return function (...args) {
        expect(this).toBe(receiver);
        expect(args).toEqual([]);
        return result;
      };
    },
  };
  expect(toLocaleString.call(receiver, "en", {})).toBe(result);
  expectThrow(() => toLocaleString.call({ toString: 1 }), typeError);
  expectThrow(() => toLocaleString.call(null), typeError);
  expectThrow(() => toLocaleString.call({ get toString() { throw "getter"; } }),
    error => expect(error).toBe("getter"));
  expectThrow(() => toLocaleString.call({ toString() { throw "call"; } }),
    error => expect(error).toBe("call"));

  expect(Object.getOwnPropertyDescriptor(Object.prototype, "__proto__")).toBe(undefined);
  expect(Object.getPrototypeOf(child)).toBe(parent);
}

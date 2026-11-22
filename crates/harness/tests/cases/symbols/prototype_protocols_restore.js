// @pending ["wait:symbol-prototype"]
// @restore
// @fulfill ["wait:symbol-prototype", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const symbol = Symbol("key");
  const box = Object(symbol);
  const method = Symbol.prototype[Symbol.toPrimitive];
  const unscopables = Array.prototype[Symbol.unscopables];
  const object = { [Symbol.unscopables]: unscopables };
  await wait("symbol-prototype");

  expect(object[Symbol.unscopables]).toBe(Array.prototype[Symbol.unscopables]);
  expect(method).toBe(Symbol.prototype[Symbol.toPrimitive]);
  expect(method === Symbol.prototype.valueOf).toBe(false);
  expect(method.name).toBe("[Symbol.toPrimitive]");
  expect(method.length).toBe(1);
  expect(method.call(symbol)).toBe(symbol);
  expect(method.call(box, { toString() { throw "hint was coerced"; } })).toBe(symbol);
  const keyed = { [box]: 42 };
  expect(keyed[symbol]).toBe(42);
  for (const receiver of [undefined, {}, 7, Symbol.prototype]) {
    let thrown;
    try {
      method.call(receiver);
    } catch (error) {
      thrown = error;
    }
    expect(thrown?.name).toBe("TypeError");
  }
  const tag = Object.getOwnPropertyDescriptor(Symbol.prototype, Symbol.toStringTag);
  expect(tag.value).toBe("Symbol");
  expect(Object.prototype.toString.call(box)).toBe("[object Symbol]");
  for (const key of [Symbol.toPrimitive, Symbol.toStringTag]) {
    const descriptor = Object.getOwnPropertyDescriptor(Symbol.prototype, key);
    expect(descriptor.writable || descriptor.enumerable).toBe(false);
    expect(descriptor.configurable).toBe(true);
  }
  expect(Object.hasOwn(Number.prototype, Symbol.toPrimitive)).toBe(false);
}

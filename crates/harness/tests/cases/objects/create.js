// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const key = Symbol("inherited");
  const proto = Object.create(null, {
    [key]: { value: 8 },
    count: { value: 2, writable: true },
    score: {
      get() { return this.count + 1; },
      set(value) { this.count = value - 1; },
    },
  });
  expect(Object.getPrototypeOf(proto)).toBe(null);
  expect("toString" in proto).toBe(false);

  const descriptors = Object.create({ ignored: { value: 10 } });
  descriptors.hidden = { value: 4 };
  const child = Object.create(proto, descriptors);
  const hidden = Object.getOwnPropertyDescriptor(child, "hidden");
  expect(hidden.value).toBe(4);
  expect(hidden.enumerable).toBe(false);
  expect(hidden.configurable).toBe(false);
  expect(hidden.writable).toBe(false);
  expect(Reflect.getPrototypeOf(child)).toBe(proto);
  expect("ignored" in child).toBe(false);
  expect("toString" in child).toBe(false);
  expect(key in child).toBe(true);
  expect(child[key]).toBe(8);
  expect("score" in child).toBe(true);
  expect(child.score).toBe(3);
  expect(Object.getOwnPropertyDescriptor(child, key)).toBe(undefined);

  child.score = 12;
  expect(child.score).toBe(12);
  expect(child.count).toBe(11);
  expect(proto.count).toBe(2);
  expect(Object.getOwnPropertyDescriptor(child, "score")).toBe(undefined);
  let inheritedWriteError;
  try {
    child[key] = 9;
  } catch (caught) {
    inheritedWriteError = caught;
  }
  expect(inheritedWriteError?.name).toBe("TypeError");
  expect(child[key]).toBe(8);
  expect(Object.getOwnPropertyDescriptor(child, key)).toBe(undefined);

  const marker = {};
  let descriptorError;
  try {
    Object.create(null, { get bad() { throw marker; } });
  } catch (caught) {
    descriptorError = caught;
  }
  expect(descriptorError).toBe(marker);
  expect(Object.getPrototypeOf(Object.create(null, undefined))).toBe(null);
}

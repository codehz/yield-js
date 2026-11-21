// @pending ["wait:prototype"]
// @restore
// @fulfill ["wait:prototype", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const child = Object.create(Object.create(Object.create(null, {
    value: { value: 21, writable: true },
    doubled: { get() { return this.value * 2; } },
  })));
  const existing = [1];
  const arrayPrototype = Object.getPrototypeOf(existing);
  Object.setPrototypeOf(arrayPrototype, Object.create(Object.getPrototypeOf(arrayPrototype), {
    restoredArrayValue: { value: 17 },
  }));

  await wait("prototype");

  expect("doubled" in child).toBe(true);
  expect(child.doubled).toBe(42);
  expect("toString" in child).toBe(false);
  const leaf = Object.getPrototypeOf(Object.getPrototypeOf(child));
  expect(Object.getPrototypeOf(leaf)).toBe(null);
  expect(leaf.value).toBe(21);
  child.value = 6;
  expect(child.doubled).toBe(12);
  expect(leaf.value).toBe(21);

  const fresh = [];
  expect(Object.getPrototypeOf(existing)).toBe(arrayPrototype);
  expect(Object.getPrototypeOf(fresh)).toBe(arrayPrototype);
  expect(existing.restoredArrayValue).toBe(17);
  expect(fresh.restoredArrayValue).toBe(17);
  const sharedParent = Object.getPrototypeOf(arrayPrototype);
  expect(Object.getPrototypeOf(sharedParent)).toBe(Object.prototype);
  expect(Reflect.setPrototypeOf(arrayPrototype, Object.prototype)).toBe(true);
  expect("restoredArrayValue" in existing).toBe(false);
  expect("restoredArrayValue" in fresh).toBe(false);
  expect(existing[0]).toBe(1);
}

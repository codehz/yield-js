// @pending ["wait:prototype"]
// @restore
// @fulfill ["wait:prototype", null]
// @fulfilled true

import { wait } from 'test';

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

  if (!("doubled" in child) || child.doubled !== 42 || "toString" in child) return false;
  const leaf = Object.getPrototypeOf(Object.getPrototypeOf(child));
  if (Object.getPrototypeOf(leaf) !== null || leaf.value !== 21) return false;
  child.value = 6;
  if (child.doubled !== 12 || leaf.value !== 21) return false;

  const fresh = [];
  if (Object.getPrototypeOf(existing) !== arrayPrototype || Object.getPrototypeOf(fresh) !== arrayPrototype) return false;
  if (existing.restoredArrayValue !== 17 || fresh.restoredArrayValue !== 17) return false;
  const sharedParent = Object.getPrototypeOf(arrayPrototype);
  if (Object.getPrototypeOf(sharedParent) !== Object.prototype) return false;
  if (!Reflect.setPrototypeOf(arrayPrototype, Object.prototype)) return false;
  return !("restoredArrayValue" in existing) && !("restoredArrayValue" in fresh) && existing[0] === 1;
}

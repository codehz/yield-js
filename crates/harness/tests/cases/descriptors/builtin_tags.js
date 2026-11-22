// @returns true

import { assert } from "test";

export function run() {
  for (const [object, tag] of [[Math, "Math"], [JSON, "JSON"], [Reflect, "Reflect"], [Promise.prototype, "Promise"]]) {
    const d = Object.getOwnPropertyDescriptor(object, Symbol.toStringTag);
    assert(d.value === tag && !d.enumerable && !d.writable && d.configurable);
  }
  assert(TypeError.isError(new TypeError()));
  assert(Object.getPrototypeOf(AggregateError) === Error);
  assert(Object.getOwnPropertyDescriptor(TypeError.prototype, "message").value === "");
  return true;
}

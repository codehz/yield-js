// @returns true

import { assert, gc } from "test";

export function run() {
  const map = new WeakMap();
  const key = Symbol("key");
  assert(map.getOrInsert(key, undefined) === undefined);
  assert(map.has(key));
  assert(map.getOrInsertComputed(key, () => { throw "called"; }) === undefined);
  const obj = {};
  function compute(actual) {
    assert(this === undefined && actual === obj);
    map.set(obj, 1);
    gc();
    return 2;
  }
  assert(map.getOrInsertComputed(obj, compute) === 2 && map.get(obj) === 2);
  try { map.getOrInsert(Symbol.for("key"), 1); } catch (e) { return Error.isError(e); }
  return false;
}

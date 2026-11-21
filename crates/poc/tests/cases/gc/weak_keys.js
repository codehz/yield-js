// @returns 42

import { gc } from "test";

function addDeadKey(map) {
  const key = Symbol("dead key");
  map.set(key, { value: -1 });
}

export function run() {
  const map = new WeakMap();
  const key = Symbol("live key");
  map.set(key, { value: 42 });
  addDeadKey(map);
  gc();
  return map.get(key).value;
}

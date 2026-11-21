// @returns true

import { gc } from "test";

function leaveCycle() {
  const object = {};
  object.self = object;
  return 1;
}

export function run() {
  leaveCycle();
  return gc() > 0;
}

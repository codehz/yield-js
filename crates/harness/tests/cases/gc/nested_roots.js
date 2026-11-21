// @returns 42

import { expect, gc } from "test";

function collectThroughGetter() {
  return ({ get value() { return gc(); } }).value;
}

function outer() {
  const key = Symbol("frame key");
  const object = {};
  object[key] = 42;
  collectThroughGetter();
  return object[key];
}

export function run() {
  expect(() => {
    gc();
    throw "still running";
  }).toThrow(/still running/);
  return outer();
}

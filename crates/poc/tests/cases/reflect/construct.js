// @returns undefined

import { expect } from 'test';

function Target(a, b) { this.sum = a + b; return 77; }
function Other() { throw 'newTarget body must not run'; }

export function run() {
  const construct = Reflect.construct;
  const instance = construct(Target, {0: 20, 1: 22, length: '2'}, Other);
  expect(instance.sum).toBe(42);
  expect(instance instanceof Other).toBe(true);
  expect(instance instanceof Target).toBe(false);
  expect(Object.getPrototypeOf(instance)).toBe(Other.prototype);
  expect(construct(Target, [1, 2]) instanceof Target).toBe(true);
  const replacement = {};
  function ReturnsObject() { return replacement; }
  expect(construct(ReturnsObject, [], Other)).toBe(replacement);
  const array = construct(Array, [3, 4], Other);
  expect(Array.isArray(array)).toBe(true);
  expect(array.length).toBe(2);
  expect(array[1]).toBe(4);
  expect(array instanceof Other).toBe(true);
  const error = construct(TypeError, ['oops'], Other);
  expect(error.message).toBe('oops');
  expect(error instanceof Other).toBe(true);
  expect(error instanceof TypeError).toBe(false);
  const argument = {};
  const object = construct(Object, [argument], Other);
  expect(object !== argument).toBe(true);
  expect(object instanceof Other).toBe(true);
  expect(Object.getPrototypeOf(argument)).toBe(Object.prototype);
  const promise = construct(Promise, [resolve => resolve(42)], Other);
  expect(promise instanceof Other).toBe(true);
  expect(construct(Promise, [resolve => resolve(42)]) instanceof Promise).toBe(true);
  Other.prototype = 0;
  function Fallback() {}
  Fallback.prototype = 0;
  expect(Object.getPrototypeOf(construct(Fallback, [], Other))).toBe(Object.getPrototypeOf(new Fallback()));
  expect(Object.getPrototypeOf(construct(Array, [], Other))).toBe(Array.prototype);
}

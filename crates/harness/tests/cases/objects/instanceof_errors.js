// @returns undefined

import { expect } from 'test';

function throwsTypeError(value, constructor) {
  try {
    value instanceof constructor;
    return false;
  } catch (e) {
    return e instanceof TypeError;
  }
}

export function run() {
  function C() {}
  C.prototype = 1;
  const arrow = () => {};
  const method = { m() {} }.m;
  const asyncFunction = async () => {};
  expect(throwsTypeError({}, {})).toBe(true);
  expect(throwsTypeError(0, null)).toBe(true);
  expect(throwsTypeError({}, C)).toBe(true);
  expect(throwsTypeError({}, arrow)).toBe(true);
  expect(throwsTypeError({}, method)).toBe(true);
  expect(throwsTypeError({}, asyncFunction)).toBe(true);
  expect(throwsTypeError({}, arrow.bind(null))).toBe(true);
}

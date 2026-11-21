// @returns undefined

import { expect } from 'test';

export function run() {
  expect(typeof Math).toBe("object");
  expect(Object.getPrototypeOf(Math)).toBe(Object.prototype);
  expect(Math.prototype).toBe(undefined);
  expect(Object.keys(Math).length).toBe(0);
  const constants = {
    E: 2.718281828459045, LN10: 2.302585092994046, LN2: 0.6931471805599453,
    LOG10E: 0.4342944819032518, LOG2E: 1.4426950408889634, PI: 3.141592653589793,
    SQRT1_2: 0.7071067811865476, SQRT2: 1.4142135623730951
  };
  for (const key of Object.keys(constants)) {
    const d = Object.getOwnPropertyDescriptor(Math, key);
    expect(d.value === constants[key]).toBe(true);
    expect(!d.writable).toBe(true);
    expect(!d.enumerable).toBe(true);
    expect(!d.configurable).toBe(true);
  }
  let assignmentError;
  try {
    Math.PI = 0;
  } catch (error) {
    assignmentError = error;
  }
  expect(assignmentError instanceof TypeError).toBe(true);

  let callError;
  try {
    Math();
  } catch (error) {
    callError = error;
  }
  expect(callError instanceof TypeError).toBe(true);

  let constructError;
  try {
    new Math();
  } catch (error) {
    constructError = error;
  }
  expect(constructError instanceof TypeError).toBe(true);
}

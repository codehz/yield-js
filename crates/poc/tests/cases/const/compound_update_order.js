// @returns undefined

import { expect } from 'test';

export function run() {
  let trace = "";
  const value = {
    valueOf() {
      trace += "v";
      return 2;
    }
  };
  const original = value;
  function rhs() {
    trace += "r";
    return 3;
  }

  let compoundError;
  try {
    value += rhs();
  } catch (error) {
    compoundError = error;
  }
  expect(compoundError instanceof TypeError).toBe(true);
  expect(trace).toBe("rv");

  let incrementError;
  try {
    value++;
  } catch (error) {
    incrementError = error;
  }
  expect(incrementError instanceof TypeError).toBe(true);
  expect(trace).toBe("rvv");

  let decrementError;
  try {
    --value;
  } catch (error) {
    decrementError = error;
  }
  expect(decrementError instanceof TypeError).toBe(true);
  expect(trace).toBe("rvvv");
  expect(value).toBe(original);
}

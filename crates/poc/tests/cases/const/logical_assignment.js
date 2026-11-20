// @returns undefined

import { expect } from 'test';

export function run() {
  const truthy = 7;
  const falsy = 0;
  const nil = null;
  let trace = "";
  function rhs(mark) {
    trace += mark;
    return 9;
  }

  expect(truthy ||= rhs("skip")).toBe(7);
  expect(falsy &&= rhs("skip")).toBe(0);
  expect(falsy ??= rhs("skip")).toBe(0);
  expect(trace).toBe("");

  let andError;
  try {
    truthy &&= rhs("a");
  } catch (error) {
    andError = error;
  }
  expect(andError instanceof TypeError).toBe(true);
  expect(trace).toBe("a");

  let orError;
  try {
    falsy ||= rhs("o");
  } catch (error) {
    orError = error;
  }
  expect(orError instanceof TypeError).toBe(true);
  expect(trace).toBe("ao");

  let nullishError;
  try {
    nil ??= rhs("n");
  } catch (error) {
    nullishError = error;
  }
  expect(nullishError instanceof TypeError).toBe(true);
  expect(trace).toBe("aon");
  expect(truthy).toBe(7);
  expect(falsy).toBe(0);
  expect(nil).toBe(null);
}

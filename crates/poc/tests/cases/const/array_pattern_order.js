// @returns undefined

import { expect } from 'test';

export function run() {
  const first = 1;
  const second = 2;
  let assigned = 0;
  let trace = "";
  let next = 0;
  const source = {
    [Symbol.iterator]() { trace += "i"; return this; },
    next() {
      trace += "n";
      next++;
      const item = next;
      return { get value() { trace += "v"; return item + 9; } };
    },
    return() { trace += "c"; throw "close"; }
  };

  // Stop after reading the second value, preserving the first write and closing the iterator.
  let writeError;
  try {
    [assigned, first, second] = source;
  } catch (error) {
    writeError = error;
  }
  expect(writeError instanceof TypeError).toBe(true);
  expect(trace).toBe("invnvc");
  expect(next).toBe(2);
  expect(assigned).toBe(10);
  expect(first).toBe(1);
  expect(second).toBe(2);
}

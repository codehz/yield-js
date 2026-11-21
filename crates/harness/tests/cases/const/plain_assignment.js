// @returns undefined

import { expect } from 'test';

export function run() {
  const value = 7;
  let calls = 0;
  function rhs() {
    calls++;
    return 8;
  }

  let writeError;
  try {
    value = rhs();
  } catch (error) {
    writeError = error;
  }
  expect(writeError instanceof TypeError).toBe(true);
  expect(writeError.message).toBe("Assignment to constant variable.");

  let rhsError;
  try {
    value = (function () { throw "rhs"; })();
  } catch (error) {
    rhsError = error;
  }
  expect(rhsError).toBe("rhs");
  expect(value).toBe(7);
  expect(calls).toBe(1);
}

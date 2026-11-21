// @returns undefined

import { expect } from 'test';

export function run() {
  const first = 1;
  const second = 2;
  let trace = "";
  const target = { set value(value) { trace += value; } };
  function receiver() { trace += "m"; return target; }
  const source = {
    a: "s",
    nested: {
      get b() { trace += "b"; return undefined; },
      get c() { trace += "c"; return 4; }
    }
  };
  function fallback() { trace += "d"; return 3; }

  let writeError;
  try {
    ({ a: receiver().value, nested: { b: first = fallback(), c: second } } = source);
  } catch (error) {
    writeError = error;
  }
  expect(writeError instanceof TypeError).toBe(true);
  expect(trace).toBe("msbd");
  expect(first).toBe(1);
  expect(second).toBe(2);
}

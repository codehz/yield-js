// @pending ["wait:args"]
// @restore
// @fulfill ["wait:args", 2]
// @fulfilled undefined

import { expect, wait } from 'test';

async function f() {
  const alias = arguments;
  const iter = arguments[Symbol.iterator]();
  const first = iter.next().value;
  const n = await wait("args");
  arguments[1] += n;
  expect(alias).toBe(arguments);
  expect(first).toBe(4);
  expect(iter.next().value).toBe(7);
  expect(alias.length).toBe(2);
  expect(alias[0]).toBe(4);
  expect(iter.next().done).toBeTruthy();
}

export function run() { return f(4, 5); }

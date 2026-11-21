// @returns undefined

import { expect } from 'test';

export function run() {
  let x = 4;
  const obj = { x: 10 };

  const a = (x)++;          // postfix: old value 4
  const b = ++(x);          // prefix: new value 6
  const c = (obj.x)++;      // postfix: old member value 10
  const d = --(obj.x);      // prefix: new member value 10
  const e = ((x))++;        // nested parens, postfix: old value 6
  const f = ++(((obj.x)));  // nested parens, prefix: new value 11

  expect(a).toBe(4);
  expect(b).toBe(6);
  expect(c).toBe(10);
  expect(d).toBe(10);
  expect(e).toBe(6);
  expect(f).toBe(11);
  expect(x).toBe(7);
  expect(obj.x).toBe(11);
}

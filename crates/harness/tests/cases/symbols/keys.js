// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const a = Symbol("x");
  const b = Symbol("x");
  const obj = { x: 0 };
  obj[a] = 1;
  obj[b] = 2;
  obj.x = 4;
  expect(obj[a]).toBe(1);
  expect(obj[b]).toBe(2);
  expect(obj.x).toBe(4);
  const zero = Symbol("0");
  const arr = [];
  arr[zero] = 9;
  expect(arr.length === 0).toBe(true);
  expect(arr[zero]).toBe(9);
  const marker = Symbol("prop");
  const holder = {
    n: 0,
    get [marker]() { return this.n; },
    set [marker](value) { this.n = value; },
  };
  holder[marker] = 5;
  expect(holder[marker]).toBe(5);
  expect(holder.hasOwnProperty(marker)).toBe(true);
}

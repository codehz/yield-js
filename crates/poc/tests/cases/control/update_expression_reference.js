// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let log = "";
  let obj = {v: 8};
  function base() { log += "b"; return obj; }
  function key() { log += "k"; return "v"; }
  let before = base()[key()]--;
  let after = ++obj.v;
  expect(log).toBe("bk");
  expect(before).toBe(8);
  expect(after).toBe(8);
  expect(obj.v).toBe(8);
}

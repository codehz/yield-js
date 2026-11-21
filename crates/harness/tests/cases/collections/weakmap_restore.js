// @pending ["wait:weakmap"]
// @restore
// @fulfill ["wait:weakmap", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const root = {};
  const next = Symbol();
  const tail = new WeakMap([[next, { marker: Symbol("live"), root }]]);
  const head = new WeakMap([[root, next]]);
  const members = new WeakSet([next]);
  await wait("weakmap");
  const entry = head.get(root);
  const value = tail.get(entry);
  expect(tail.has(entry)).toBe(true);
  expect(members.has(entry)).toBe(true);
  expect(typeof value.marker).toBe("symbol");
  expect(value.root).toBe(root);
}

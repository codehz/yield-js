// @pending ["wait:weakmap"]
// @restore
// @fulfill ["wait:weakmap", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const root = {};
  const next = {};
  const tail = new WeakMap([[next, { marker: Symbol("live"), root }]]);
  const head = new WeakMap([[root, { next }]]);
  await wait("weakmap");
  const entry = head.get(root);
  const value = tail.get(entry.next);
  expect(tail.has(entry.next)).toBe(true);
  expect(typeof value.marker).toBe("symbol");
  expect(value.root).toBe(root);
}

// @pending ["wait:resource-node"]
// @restore
// @fulfill ["wait:resource-node", 42]
// @fulfilled undefined

import { bind, expect } from 'test';

export async function run() {
  const node = bind('resource-node');
  const sync = node[Symbol.dispose];
  const asyncDispose = node[Symbol.asyncDispose];
  expect(Symbol.dispose.description).toBe('Symbol.dispose');
  expect(Symbol.asyncDispose.description).toBe('Symbol.asyncDispose');
  expect(sync.call(node)).toBe('resource-node');
  for (const method of [sync, asyncDispose]) {
    let rejectedReceiver = false;
    try {
      method.call({});
    } catch (error) {
      rejectedReceiver = error instanceof TypeError;
    }
    expect(rejectedReceiver).toBe(true);
  }
  expect(await asyncDispose.call(node)).toBe(42);
  expect(node[Symbol.dispose]).toBe(sync);
  expect(node[Symbol.asyncDispose]).toBe(asyncDispose);
  expect(node[Symbol.dispose]()).toBe('resource-node');
}

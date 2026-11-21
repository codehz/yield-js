// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

import { bind, expect, wait } from 'test';

export async function run() {
  const node = bind('persisted');
  await wait('a');
  expect(node.label()).toBe('persisted');
}

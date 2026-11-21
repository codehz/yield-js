// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 0]
// @fulfilled undefined

import { bind, expect, wait } from 'test';

export async function run() {
  const node = bind('alpha');
  const primitive = node[Symbol.toPrimitive];
  await wait('a');
  expect(node[Symbol.toPrimitive]).toBe(primitive);
  expect(primitive.call(node, 'string')).toBe('alpha');
}

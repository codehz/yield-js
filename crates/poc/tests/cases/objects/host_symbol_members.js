// @returns true

import { assert, bind } from 'test';

export function run() {
  const node = bind('alpha');
  const primitive = node[Symbol.toPrimitive];
  assert(typeof primitive === 'function');
  assert(primitive === node[Symbol.toPrimitive]);
  assert(primitive.call(node, 'string') === 'alpha');
  assert(primitive.call(node, 'number') === 'alpha');
  return true;
}

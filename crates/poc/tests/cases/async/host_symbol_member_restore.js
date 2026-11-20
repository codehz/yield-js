// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 0]
// @fulfilled true

import { assert, bind, wait } from 'test';

export async function run() {
  const node = bind('alpha');
  const primitive = node[Symbol.toPrimitive];
  await wait('a');
  assert(node[Symbol.toPrimitive] === primitive);
  assert(primitive.call(node, 'string') === 'alpha');
  return true;
}

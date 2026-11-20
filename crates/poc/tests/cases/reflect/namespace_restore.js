// @pending ["wait:namespaces"]
// @restore
// @fulfill ["wait:namespaces", 0]
// @fulfilled 4

import { wait } from 'test';

export async function run() {
  const max = Math.max;
  const get = Reflect.get;
  Math.max = () => -1;
  Reflect.get = () => -1;
  await wait("namespaces");
  return max(get({x: 4}, "x"), 2);
}

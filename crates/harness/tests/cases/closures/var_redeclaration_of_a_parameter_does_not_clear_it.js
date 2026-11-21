// @args [5]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled 5

import { wait } from 'test';

export async function run(x) {
  var x;
  await wait("a");
  return x;
}

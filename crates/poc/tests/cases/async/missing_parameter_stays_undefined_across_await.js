// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

import { wait } from 'test';

export async function run(x) {
  await wait("a");
  return x;
}

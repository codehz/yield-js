// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 0]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const fn = String.prototype.slice;
  fn.tag = 7;
  await wait('a');
  return fn === String.prototype.slice && fn.tag === 7 && fn.call('abc', 1) === 'bc';
}

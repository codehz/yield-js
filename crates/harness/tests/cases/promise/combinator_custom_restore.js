// @pending ["wait:input"]
// @restore
// @fulfill ["wait:input", 2]
// @fulfilled "1,2"

import { gc, wait } from 'test';

export async function run() {
  let deliver;
  const completion = new Promise(resolve => { deliver = resolve; });
  const result = {};
  function C(executor) {
    executor(values => deliver(values.join(',')), () => deliver('unexpected rejection'));
    return result;
  }
  C.resolve = value => ({ then(accept) {
    if (value === 1) accept(1);
    else wait('input').then(v => { gc(); accept(v); accept(99); });
  } });
  if (Promise.all.call(C, [1, 2]) !== result) return 'wrong identity';
  gc();
  return await completion;
}

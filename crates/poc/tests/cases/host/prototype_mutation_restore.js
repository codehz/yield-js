// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled true

import { expect, wait } from 'test';

export async function run() {
  const matcher = expect(1);
  const prototype = Object.getPrototypeOf(matcher);
  prototype.toBe = function () { return 'patched'; };
  delete prototype.toEqual;
  await wait('a');
  return matcher.toBe(2) === 'patched'
    && typeof matcher.toEqual === 'undefined'
    && Object.getPrototypeOf(matcher) === prototype;
}

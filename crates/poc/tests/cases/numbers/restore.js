// @pending ["wait:number"]
// @restore
// @fulfill ["wait:number", null]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const constructor = Number;
  const prototype = Number.prototype;
  await wait("number");
  return constructor === Number && prototype === Number.prototype
    && prototype.constructor === Number && (1).valueOf === prototype.valueOf
    && Number.isNaN(Number.NaN) && Number.POSITIVE_INFINITY === Infinity
    && Number.NEGATIVE_INFINITY === -Infinity;
}

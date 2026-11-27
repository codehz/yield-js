// @fulfilled true

import { gc, syncCall } from 'test';

export async function run() {
  const receiver = {};
  const arg = {};
  const order = [];
  Promise.resolve().then(() => order.push('microtask'));

  const wrapped = syncCall(function (outerArg) {
    order.push('outer');
    const inner = syncCall(function (innerArg) {
      order.push('inner');
      gc();
      return this === receiver && innerArg === arg ? innerArg : null;
    }, this, outerArg);
    order.push('outer returned');
    return this === receiver && outerArg === arg ? inner : null;
  }, receiver, arg);
  order.push('both returned');
  if (wrapped.result.result !== arg) return false;
  await Promise.resolve();
  if (order.join(',') !== 'outer,inner,outer returned,both returned,microtask') return false;

  const thrown = {};
  try {
    syncCall(() => syncCall(() => { throw thrown; }, undefined, undefined), undefined, undefined);
    return false;
  } catch (error) {
    if (error !== thrown) return false;
  }

  try {
    syncCall({}, undefined, undefined);
    return false;
  } catch (error) {
    if (!(error instanceof TypeError)) return false;
  }

  let entered = 0;
  function recurse() {
    entered++;
    return syncCall(recurse, undefined, undefined);
  }
  try {
    syncCall(recurse, undefined, undefined);
    return false;
  } catch (error) {
    if (!(error instanceof RangeError) || error.message !== 'host callback nesting limit exceeded') return false;
    if (entered !== 16) return false;
  }

  const promise = Promise.resolve(arg);
  return syncCall(() => promise, undefined, undefined).result === promise;
}

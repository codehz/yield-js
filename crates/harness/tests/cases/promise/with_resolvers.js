// @pending ["wait:capability"]
// @restore
// @fulfill ["wait:capability", null]
// @fulfilled true

import { expect, wait } from 'test';

export async function run() {
  const withResolvers = Promise.withResolvers;
  expect(withResolvers.length).toBe(0);
  const cap = withResolvers.call(Promise);
  expect(Object.getPrototypeOf(cap) === Object.prototype).toBe(true);
  expect(Object.keys(cap).join(',')).toBe('promise,resolve,reject');
  expect(cap.promise instanceof Promise).toBe(true);

  const promise = {};
  const resolve = () => {};
  const reject = () => {};
  function C(executor) {
    executor(resolve, reject);
    return promise;
  }
  const custom = withResolvers.call(C);
  expect(custom.promise === promise && custom.resolve === resolve && custom.reject === reject).toBe(true);
  try {
    withResolvers.call(() => {});
    return false;
  } catch (error) {
    expect(error instanceof TypeError).toBe(true);
  }

  await wait('capability');
  const second = withResolvers.call(Promise);
  second.resolve({ then(resolve) { resolve(7); } });
  second.reject('ignored');
  expect(await second.promise).toBe(7);
  cap.reject('first');
  cap.resolve('ignored');
  try {
    await cap.promise;
    return false;
  } catch (error) {
    return error === 'first';
  }
}

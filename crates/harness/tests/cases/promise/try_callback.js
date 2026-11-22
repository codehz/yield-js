// @fulfilled true

import { expect, gc } from 'test';

export async function run() {
  expect(Promise.try.length).toBe(1);
  let order = '';
  const promise = Promise.try(function (a, b) {
    expect(this).toBe(undefined);
    gc();
    order += 'callback';
    return { then(resolve) { resolve(a + b); } };
  }, 2, 3);
  expect(order).toBe('callback');
  expect(await promise).toBe(5);

  const reason = {};
  const rejected = Promise.try(() => { throw reason; });
  try {
    await rejected;
    return false;
  } catch (error) {
    expect(error === reason).toBe(true);
  }
  try {
    await Promise.try();
    return false;
  } catch (error) {
    expect(error instanceof TypeError).toBe(true);
  }

  let called = false;
  try {
    Promise.try.call({}, () => { called = true; });
    return false;
  } catch (error) {
    expect(error instanceof TypeError && !called).toBe(true);
  }

  const result = {};
  let received;
  function C(executor) {
    called = true;
    executor(function (value) {
      expect(this).toBe(undefined);
      received = value;
    }, function (error) {
      expect(this).toBe(undefined);
      throw error;
    });
    return result;
  }
  const custom = Promise.try.call(C, () => called);
  expect(custom === result && received === true).toBe(true);
  try {
    Promise.try.call(C, () => { throw reason; });
    return false;
  } catch (error) {
    return error === reason;
  }
}

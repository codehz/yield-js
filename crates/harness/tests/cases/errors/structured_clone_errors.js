// @returns true
import { expect } from 'test';

function thrown(callback) {
  try {
    callback();
  } catch (error) {
    return error;
  }
  throw 'expected an exception';
}

function assertDataCloneError(value) {
  const error = thrown(() => structuredClone(value));
  expect(error instanceof DOMException).toBe(true);
  expect(error instanceof Error).toBe(true);
  expect(error.name).toBe('DataCloneError');
  expect(error.code).toBe(25);
}

export function run() {
  expect(thrown(() => structuredClone()) instanceof TypeError).toBe(true);
  expect(structuredClone(undefined)).toBe(undefined);
  assertDataCloneError(() => {});
  assertDataCloneError(Symbol('value'));
  const pair = Proxy.revocable({}, {});
  pair.revoke();
  assertDataCloneError(pair.proxy);
  const buffer = new ArrayBuffer(8, { maxByteLength: 16 });
  const view = new Uint8Array(buffer, 4, 4);
  buffer.resize(2);
  assertDataCloneError(view);

  const invalidDate = new Date(NaN);
  invalidDate.extra = () => {};
  const dateCopy = structuredClone(invalidDate);
  expect(dateCopy instanceof Date).toBe(true);
  expect(Number.isNaN(dateCopy.getTime())).toBe(true);
  expect(Object.hasOwn(dateCopy, 'extra')).toBe(false);
  const pattern = /x+/gi;
  pattern.lastIndex = 3;
  const patternCopy = structuredClone(pattern);
  expect(patternCopy instanceof RegExp).toBe(true);
  expect(patternCopy.source).toBe('x+');
  expect(patternCopy.flags).toBe('gi');
  expect(patternCopy.lastIndex).toBe(0);
  expect(pattern.lastIndex).toBe(3);

  const error = new TypeError('original');
  error.cause = { owner: error };
  const errorCopy = structuredClone(error);
  expect(errorCopy === error).toBe(false);
  expect(errorCopy instanceof TypeError).toBe(true);
  expect(errorCopy.message).toBe('original');
  expect(errorCopy.cause === error.cause).toBe(false);
  expect(errorCopy.cause.owner).toBe(errorCopy);
  return true;
}

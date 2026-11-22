// @pending ["wait:clone"]
// @restore
// @fulfill ["wait:clone", true]
// @fulfilled true
import { expect, gc, wait } from 'test';

function checkException(value) {
  expect(Object.getPrototypeOf(value)).toBe(DOMException.prototype);
  expect(value instanceof Error).toBe(true);
  expect(value.name).toBe('DataCloneError');
  expect(value.message).toBe('internal message');
  expect(value.code).toBe(25);
  expect(Object.hasOwn(value, 'name')).toBe(false);
  expect(Object.hasOwn(value, 'message')).toBe(false);
}

function checkForgedBrand() {
  const fake = Object.create(DOMException.prototype);
  expect(fake instanceof DOMException).toBe(true);
  const names = ['name', 'message', 'code'];
  for (const name of names) {
    const getter = Object.getOwnPropertyDescriptor(DOMException.prototype, name).get;
    let caught = false;
    try {
      getter.call(fake);
    } catch (error) {
      caught = error instanceof TypeError;
    }
    expect(caught).toBe(true);
  }
}

export async function run() {
  const original = new DOMException('internal message', 'DataCloneError');
  Object.defineProperty(original, 'name', { value: 'spoofed name' });
  Object.defineProperty(original, 'message', { get() { throw 'spoofed message read'; } });
  const copy = structuredClone({ first: original, second: original });
  expect(copy.first === original).toBe(false);
  expect(copy.first).toBe(copy.second);
  checkException(copy.first);
  checkForgedBrand();

  expect(await wait('clone')).toBe(true);
  gc();
  expect(original.name).toBe('spoofed name');
  expect(copy.first === original).toBe(false);
  expect(copy.first).toBe(copy.second);
  checkException(copy.first);
  checkException(structuredClone(original));
  checkForgedBrand();
  return true;
}

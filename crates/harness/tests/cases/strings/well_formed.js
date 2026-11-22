// @returns undefined

import { expect } from 'test';

export function run() {
  expect(''.isWellFormed()).toBe(true);
  expect('é😀\uFFFD'.isWellFormed()).toBe(true);
  expect('\uD800'.isWellFormed()).toBe(false);
  expect('\uDFFF'.isWellFormed()).toBe(false);
  const mixed = 'é😀\uD800\uD801\uDC00\uDC01x\uD800';
  expect(mixed.isWellFormed()).toBe(false);
  expect(mixed.toWellFormed()).toBe('é😀\uFFFD\uD801\uDC00\uFFFDx\uFFFD');
  expect(mixed.toWellFormed().isWellFormed()).toBe(true);
  expect(new String('😀').toWellFormed()).toBe('😀');
  expect(String.prototype.toWellFormed.call(123)).toBe('123');
  let conversions = 0;
  const receiver = { toString() { conversions++; return mixed; } };
  expect(String.prototype.isWellFormed.call(receiver)).toBe(false);
  expect(conversions).toBe(1);

  for (const method of [String.prototype.isWellFormed, String.prototype.toWellFormed]) {
    expect(method.length).toBe(0);
    for (const invalid of [null, undefined, Symbol()]) {
      let name;
      try { method.call(invalid); } catch (error) { name = error.name; }
      expect(name).toBe('TypeError');
    }
    let thrown;
    try { method.call({ toString() { throw 'coercion'; } }); }
    catch (error) { thrown = error; }
    expect(thrown).toBe('coercion');
  }
}

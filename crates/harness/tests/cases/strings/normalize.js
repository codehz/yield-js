// @returns undefined

import { expect } from 'test';

export function run() {
  expect(String.prototype.normalize.length).toBe(0);
  expect('e\u0301'.normalize()).toBe('é');
  expect('e\u0301'.normalize(undefined)).toBe('é');
  expect('é'.normalize('NFD')).toBe('e\u0301');
  expect('\uFB03\u212B'.normalize('NFKC')).toBe('ffiÅ');
  expect('\uFB03\u212B'.normalize('NFKD')).toBe('ffiA\u030A');
  expect('\u1100\u1161'.normalize('NFC')).toBe('\uAC00');
  expect('a\u0301\u0323'.normalize('NFD')).toBe('a\u0323\u0301');
  expect('e\uD800\u0301\uDC00e\u0301😀'.normalize()).toBe('e\uD800\u0301\uDC00é😀');
  let order = '';
  const receiver = { toString() { order += 'r'; return 'é'; } };
  const form = { toString() { order += 'f'; return 'NFD'; } };
  expect(String.prototype.normalize.call(receiver, form)).toBe('e\u0301');
  expect(order).toBe('rf');
  for (const invalid of ['nfc', 'NFC ', '\uD800', null]) {
    let name;
    try { ''.normalize(invalid); } catch (error) { name = error.name; }
    expect(name).toBe('RangeError');
  }
  let name;
  try { 'a'.normalize(Symbol()); } catch (error) { name = error.name; }
  expect(name).toBe('TypeError');
}

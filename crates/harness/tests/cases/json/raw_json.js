// @returns undefined

import { expect } from 'test';

export function run() {
  expect(JSON.rawJSON.name).toBe('rawJSON');
  expect(JSON.isRawJSON.name).toBe('isRawJSON');
  expect(JSON.rawJSON.length).toBe(1);
  expect(JSON.isRawJSON.length).toBe(1);
  expect(Object.prototype.propertyIsEnumerable.call(JSON, 'rawJSON')).toBe(false);
  expect(globalThis.rawJSON).toBe(undefined);
  expect(globalThis.isRawJSON).toBe(undefined);
  const raw = JSON.rawJSON(9007199254740993n);
  const proxy = new Proxy(raw, {});
  const fake = { rawJSON: raw.rawJSON };
  expect(JSON.isRawJSON(raw)).toBe(true);
  expect(JSON.isRawJSON(proxy)).toBe(false);
  expect(JSON.isRawJSON(fake)).toBe(false);
  expect(JSON.isRawJSON(Object.create(raw))).toBe(false);
  expect(JSON.isRawJSON()).toBe(false);
  expect(Object.getPrototypeOf(raw)).toBe(null);
  expect(Object.isFrozen(raw)).toBe(true);
  const descriptor = Object.getOwnPropertyDescriptor(raw, 'rawJSON');
  expect(descriptor.value).toBe('9007199254740993');
  expect(descriptor.enumerable).toBe(true);
  expect(descriptor.writable || descriptor.configurable).toBe(false);
  expect(JSON.stringify(raw)).toBe('9007199254740993');
  expect(JSON.stringify(fake)).toBe('{"rawJSON":"9007199254740993"}');
  expect(JSON.stringify(proxy)).toBe('{"rawJSON":"9007199254740993"}');
  expect(JSON.stringify([JSON.rawJSON('-0'), JSON.rawJSON('1e999'), JSON.rawJSON('"\\u0061"')])).toBe('[-0,1e999,"\\u0061"]');
  expect(JSON.stringify({ x: raw }, null, 2)).toBe('{\n  "x": 9007199254740993\n}');
  expect(JSON.stringify({ x: raw }, ['x'], raw)).toBe('{"x":9007199254740993}');
  expect(JSON.stringify(new Array(1), (key, value) => key === '0' ? raw : value)).toBe('[9007199254740993]');
  expect(JSON.stringify({ toJSON() { return raw; } })).toBe('9007199254740993');
  expect(JSON.stringify({ x: 1 }, (key, value) => key === 'x' ? raw : value)).toBe('{"x":9007199254740993}');
  expect(JSON.stringify(raw, () => 1)).toBe('1');
  expect(JSON.stringify(JSON.rawJSON({ toString() { return 'true'; } }))).toBe('true');

  BigInt.prototype.toJSON = function() { return JSON.rawJSON(this); };
  try {
    expect(JSON.stringify({ n: 9007199254740993n })).toBe('{"n":9007199254740993}');
  } finally {
    delete BigInt.prototype.toJSON;
  }

  const bad = ['', ' 1', '1 ', '\ttrue', 'null\n', '\u00a0null', '[]', '{}', '[1]', '01', '1e', 'truefalse', '"\\x00"', '"\n"', undefined, Infinity];
  for (const source of bad) {
    let error;
    try { JSON.rawJSON(source); } catch (caught) { error = caught; }
    expect(error instanceof SyntaxError).toBe(true);
  }
  let error;
  try { JSON.rawJSON(Symbol()); } catch (caught) { error = caught; }
  expect(error instanceof TypeError).toBe(true);
}

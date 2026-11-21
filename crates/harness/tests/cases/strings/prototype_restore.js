// @pending ["wait:string-prototype"]
// @restore
// @fulfill ["wait:string-prototype", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const text = String.fromCodePoint(233, 128512, 122);
  const proto = String.prototype;
  const iteratorMethod = proto[Symbol.iterator];
  const iterator = text[Symbol.iterator]();
  const first = iterator.next();
  proto.readLength = function () { return this.length; };
  proto[Symbol.iterator] = function () { return this; };
  await wait("string-prototype");
  expect(first.value).toBe("é");
  expect(!first.done).toBe(true);
  expect(typeof text).toBe("string");
  expect(String.prototype).toBe(proto);
  expect(text.readLength()).toBe(4);
  expect(text.missing).toBe(undefined);
  expect(text[Symbol.iterator]()).toBe(text);
  expect(proto[Symbol.iterator] !== iteratorMethod).toBe(true);
  expect(iterator.next().value).toBe("😀");
  expect(iterator.next().value).toBe("z");
  expect(!!iterator.next().done).toBe(true);
  expect(text.slice(1, 3)).toBe("😀");
  expect(String.fromCharCode(122)).toBe("z");
}

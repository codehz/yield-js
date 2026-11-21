// @returns undefined

import { expect, gc } from 'test';

export function run() {
  const invalid = new Date(NaN);
  expect(invalid.toString()).toBe('Invalid Date');
  expect(invalid.toJSON()).toBe(null);
  expect(JSON.stringify({ invalid })).toBe('{"invalid":null}');
  let thrown;
  try { invalid.toISOString(); } catch (error) { thrown = error; }
  expect(thrown instanceof RangeError).toBe(true);
  try { Date.prototype.getTime.call(Object.create(Date.prototype)); } catch (error) { thrown = error; }
  expect(thrown instanceof TypeError).toBe(true);

  class Sub extends Date {}
  const d = new Sub(0);
  expect(d instanceof Sub).toBe(true);
  expect(+d).toBe(0);
  expect(d + '').toBe('Thu Jan 01 1970 00:00:00 GMT+0000');
  expect(d.toTimeString()).toBe('00:00:00 GMT+0000');
  expect(d.toLocaleDateString()).toBe(d.toDateString());
  expect(d.toLocaleTimeString()).toBe(d.toTimeString());
  expect(d.toLocaleString()).toBe(d.toString());
  expect(Date.prototype.toGMTString).toBe(Date.prototype.toUTCString);
  expect(Object.prototype.toString.call(d)).toBe('[object Date]');
  Object.defineProperty(d, Symbol.toStringTag, { get() { gc(); return 'UserDate'; } });
  expect(Object.prototype.toString.call(d)).toBe('[object UserDate]');
  d.valueOf = () => { throw 'Date argument must not coerce'; };
  expect(new Date(d).getTime()).toBe(0);
  expect(JSON.stringify(new Date(0))).toBe('"1970-01-01T00:00:00.000Z"');
  expect(Date.prototype.toJSON.call({ valueOf() { return Infinity; } })).toBe(null);
  expect(Date.prototype.toJSON.call({ toISOString() { gc(); return 5; } })).toBe(5);
  const primitive = Date.prototype[Symbol.toPrimitive];
  expect(primitive.call({ toString() { return 'text'; } }, 'default')).toBe('text');
  expect(primitive.call({ valueOf() { return 6; } }, 'number')).toBe(6);
  const descriptor = Object.getOwnPropertyDescriptor(Date.prototype, Symbol.toPrimitive);
  expect(descriptor.value).toBe(primitive);
  expect(descriptor.writable).toBe(false);
  expect(descriptor.enumerable).toBe(false);
  expect(descriptor.configurable).toBe(true);
  expect(Date.prototype.setUTCFullYear.length).toBe(3);
  expect(Date.prototype.setHours.length).toBe(4);
  expect(Date.prototype.toJSON.length).toBe(1);
}

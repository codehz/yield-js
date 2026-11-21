// @returns undefined

import { expect } from 'test';

function example(a) {}

export function run() {
  const proto = Object.getPrototypeOf(example);
  expect(Object.getPrototypeOf(Array)).toBe(proto);
  expect(proto.name).toBe('');
  expect(proto.length === 0).toBe(true);
  expect(Array.name).toBe('Array');
  expect(Array.length).toBe(1);
  expect(Object.keys.name).toBe('keys');
  expect(Object.keys.length).toBe(1);

  for (const fn of [example, proto, Array, Object.keys, example.bind(null)]) {
    expect('name' in fn).toBe(true);
    expect('length' in fn).toBe(true);
    expect(Reflect.has(fn, 'name')).toBe(true);
    expect(Reflect.has(fn, 'length')).toBe(true);
    expect(Object.keys(fn).includes('name')).toBe(false);
    expect(Object.keys(fn).includes('length')).toBe(false);
    expect(Reflect.ownKeys(fn).slice(0, 2).join(',')).toBe('length,name');
    for (const key of ['length', 'name']) {
      const d = Reflect.getOwnPropertyDescriptor(fn, key);
      expect(d.value).toBe(fn[key]);
      expect(d.writable).toBe(false);
      expect(d.enumerable).toBe(false);
      expect(d.configurable).toBe(true);
    }
    expect(Reflect.set(fn, 'name', 'changed')).toBe(false);
    expect(Reflect.set(fn, 'length', 99)).toBe(false);
  }
  let threw = false;
  try { example.name = 'changed'; } catch (e) {
    expect(e instanceof TypeError).toBe(true);
    expect(example.name).toBe('example');
    expect(example.length).toBe(1);
    threw = true;
  }
  expect(threw).toBe(true);
  expect(delete example.name).toBe(true);
  expect(Object.prototype.hasOwnProperty.call(example, 'name')).toBe(false);
  expect(example.name).toBe('');
  expect(delete example.length).toBe(true);
  expect(example.length).toBe(0);
  expect(Reflect.defineProperty(example, 'name', {get() { throw 'name was read'; }})).toBe(true);
  expect(proto.toString.call(example)).toBe('function example() { [native code] }');
  expect(proto.toString.call({method() { return 42; }}.method)).toBe('function method() { [native code] }');
  expect(proto.toString.call(Object.keys)).toBe('function keys() { [native code] }');
  let invalid = false;
  try { proto.toString.call({}); } catch (e) { invalid = e instanceof TypeError; }
  expect(invalid).toBe(true);
}

// @returns undefined

import { expect } from 'test';

function example(a) {}

export function run() {
  const proto = Object.getPrototypeOf(example);
  expect(Object.getPrototypeOf(Array)).toBe(proto);
  expect(proto.name).toBe('');
  expect(proto.length === 0).toBe(true);
  expect(Array.name).toBe('Array');
  expect(Array.length === 0).toBe(true);
  expect(Object.keys.name).toBe('keys');
  expect(Object.keys.length === 0).toBe(true);

  for (const fn of [example, proto, Array, Object.keys, example.bind(null)]) {
    expect('name' in fn).toBe(true);
    expect('length' in fn).toBe(true);
    expect(Reflect.has(fn, 'name')).toBe(true);
    expect(Reflect.has(fn, 'length')).toBe(true);
    expect(Object.keys(fn).includes('name')).toBe(false);
    expect(Object.keys(fn).includes('length')).toBe(false);
    expect(Reflect.ownKeys(fn).includes('name')).toBe(false);
    expect(Reflect.ownKeys(fn).includes('length')).toBe(false);
    expect(Object.getOwnPropertyDescriptor(fn, 'name')).toBe(undefined);
    expect(Reflect.getOwnPropertyDescriptor(fn, 'length')).toBe(undefined);
    expect(Reflect.set(fn, 'name', 'changed')).toBe(false);
    expect(Reflect.set(fn, 'length', 99)).toBe(false);
  }
  try { example.name = 'changed'; } catch (e) {
    expect(e instanceof TypeError).toBe(true);
    expect(example.name).toBe('example');
    expect(example.length).toBe(1);
    return;
  }
  expect('name assignment succeeded').toBe('name assignment threw');
}

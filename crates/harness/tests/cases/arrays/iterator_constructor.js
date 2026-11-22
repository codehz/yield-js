// @returns undefined

import { expect } from 'test';

function typeError(fn) {
  let error;
  try { fn(); } catch (e) { error = e; }
  expect(error instanceof TypeError).toBe(true);
}

function dataProperty(object, key, value, writable, enumerable, configurable) {
  const desc = Object.getOwnPropertyDescriptor(object, key);
  expect(desc.value).toBe(value);
  expect(desc.writable).toBe(writable);
  expect(desc.enumerable).toBe(enumerable);
  expect(desc.configurable).toBe(configurable);
}

export function run() {
  const proto = Iterator.prototype;
  expect(Object.getPrototypeOf(Iterator)).toBe(Function.prototype);
  expect(Object.getPrototypeOf(proto)).toBe(Object.prototype);
  expect(Object.getPrototypeOf(Object.getPrototypeOf([].values()))).toBe(proto);
  dataProperty(globalThis, 'Iterator', Iterator, true, false, true);
  dataProperty(Iterator, 'prototype', proto, false, false, false);
  dataProperty(Iterator, 'name', 'Iterator', false, false, true);
  dataProperty(Iterator, 'length', 0, false, false, true);
  for (const [key, arity] of [['from', 1], ['concat', 0]]) {
    const method = Iterator[key];
    dataProperty(Iterator, key, method, true, false, true);
    expect(method.name).toBe(key);
    expect(method.length).toBe(arity);
    typeError(() => Reflect.construct(method, []));
  }
  dataProperty(proto, Symbol.iterator, proto[Symbol.iterator], true, false, true);
  expect(proto[Symbol.iterator].call(12)).toBe(12);

  typeError(() => Iterator());
  typeError(() => new Iterator());
  typeError(() => Reflect.construct(Iterator, [], Iterator));
  function Alternate() { throw new Error('newTarget must not be called'); }
  const custom = Reflect.construct(Iterator, [123], Alternate);
  expect(Object.getPrototypeOf(custom)).toBe(Alternate.prototype);
  expect(custom instanceof Iterator).toBe(false);
  Alternate.prototype = null;
  expect(Object.getPrototypeOf(Reflect.construct(Iterator, [], Alternate))).toBe(proto);

  class Counter extends Iterator {
    constructor(limit) { super(); this.limit = limit; this.index = 0; }
    next() {
      if (this.index === this.limit) return { done: true };
      return { value: this.index++, done: false };
    }
  }
  class Derived extends Counter {}
  const counter = new Derived(3);
  expect(Object.getPrototypeOf(counter)).toBe(Derived.prototype);
  expect(counter instanceof Counter).toBe(true);
  expect(counter instanceof Iterator).toBe(true);
  expect(counter[Symbol.iterator]()).toBe(counter);
  expect(counter.map(n => n * 2).toArray().join()).toBe('0,2,4');

  for (const [key, label, initial] of [
    ['constructor', 'constructor', Iterator],
    [Symbol.toStringTag, '[Symbol.toStringTag]', 'Iterator']
  ]) {
    const desc = Object.getOwnPropertyDescriptor(proto, key);
    expect(desc.enumerable).toBe(false);
    expect(desc.configurable).toBe(true);
    expect('value' in desc).toBe(false);
    expect('writable' in desc).toBe(false);
    expect(desc.get.name).toBe('get ' + label);
    expect(desc.get.length).toBe(0);
    expect(desc.set.name).toBe('set ' + label);
    expect(desc.set.length).toBe(1);
    expect(desc.get.call(null)).toBe(initial);
    typeError(() => desc.set.call(1, 'ignored'));
    typeError(() => Reflect.set(proto, key, 'ignored'));
    expect(proto[key]).toBe(initial);

    const derived = Object.create(proto);
    expect(derived[key]).toBe(initial);
    derived[key] = 'own';
    dataProperty(derived, key, 'own', true, true, true);
    expect(desc.set.call(derived, 'updated')).toBe(undefined);
    expect(derived[key]).toBe('updated');
    Object.defineProperty(derived, key, { writable: false });
    typeError(() => desc.set.call(derived, 'ignored'));
    typeError(() => desc.set.call(Object.preventExtensions(Object.create(proto)), 'ignored'));
  }
}

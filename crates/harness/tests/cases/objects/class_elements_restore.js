// @pending ["wait:computed class key"]
// @restore
// @fulfill ["wait:computed class key", "method"]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  let order = '';
  let conversions = 0;
  const symbol = Symbol('field');
  const key = { toString() { conversions++; order += 'K'; return symbol; } };
  const failure = {};
  class C {
    static first = (order += 'S', C.prototype.method);
    [key] = (order += 'I', this);
    static {
      order += 'B';
      expect(local).toBe(undefined);
      if (true) { var local = this; }
      this.readLocal = () => local;
      expect((() => new.target)()).toBe(undefined);
    }
    [await wait(order === 'K' ? 'computed class key' : 'unexpected order')]() { return C; }
    static last = (order += 'L', this);
    uninitialized;
    get value() { return this.uninitialized; }
    set value(value) { this.uninitialized = value; }
    constructor(owner = this[symbol]) { order += 'C'; expect(owner).toBe(this); }
  }
  expect(order).toBe('KSBL');
  expect(C.first).toBe(C.prototype.method);
  expect(C.last).toBe(C);
  expect(C.readLocal()).toBe(C);
  let sets = 0;
  Object.setPrototypeOf(C.prototype, { set uninitialized(value) { sets++; } });
  expect(typeof local).toBe('undefined');
  const a = new C();
  const b = new C();
  expect(sets).toBe(0);
  expect(conversions).toBe(1);
  expect(order).toBe('KSBLICIC');
  expect(a[symbol]).toBe(a);
  expect(b[symbol]).toBe(b);
  expect(a.method()).toBe(C);
  expect(a.method.name).toBe('method');
  expect(a.value).toBe(undefined);
  a.value = failure;
  expect(a.value).toBe(failure);
  const describe = Object.getOwnPropertyDescriptor;
  function flags(o, key, writable, enumerable, configurable) {
    const d = describe(o, key);
    expect(d.writable).toBe(writable);
    expect(d.enumerable).toBe(enumerable);
    expect(d.configurable).toBe(configurable);
    return d;
  }
  expect(flags(C, 'prototype', false, false, false).value).toBe(C.prototype);
  expect(flags(C.prototype, 'constructor', true, false, true).value).toBe(C);
  expect(flags(C.prototype, 'method', true, false, true).value).toBe(C.first);
  const accessor = flags(C.prototype, 'value', undefined, false, true);
  expect(typeof accessor.get).toBe('function');
  expect(typeof accessor.set).toBe('function');
  expect(flags(a, symbol, true, true, true).value).toBe(a);
  expect(flags(b, 'uninitialized', true, true, true).value).toBe(undefined);
  expect(flags(C, 'last', true, true, true).value).toBe(C);
}

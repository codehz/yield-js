// @returns undefined

import { expect } from 'test';

export function run() {
  let order = '';
  class Base {
    method() { return this.n; }
    static get owner() { return this; }
  }
  class Derived extends (order += 'H', Base) {
    [(order += 'K', 'read')]() { return this.method(); }
    static { order += 'S'; expect(this.owner).toBe(this); }
  }
  expect(order).toBe('HKS');
  expect(Object.getPrototypeOf(Derived)).toBe(Base);
  expect(Object.getPrototypeOf(Derived.prototype)).toBe(Base.prototype);
  const o = Object.create(Derived.prototype);
  o.n = 9;
  expect(o.read()).toBe(9);
  expect(o instanceof Derived).toBe(true);
  expect(o instanceof Base).toBe(true);
  class Empty extends null {}
  expect(Object.getPrototypeOf(Empty.prototype)).toBe(null);
  expect(Object.getPrototypeOf(Empty)).toBe(Object.getPrototypeOf(Base));
  function NullPrototype() {}
  NullPrototype.prototype = null;
  class AlsoEmpty extends NullPrototype {}
  expect(Object.getPrototypeOf(AlsoEmpty.prototype)).toBe(null);
  expect(Object.getPrototypeOf(AlsoEmpty)).toBe(NullPrototype);
  let prototypeReads = 0;
  const Bound = NullPrototype.bind(undefined);
  Object.defineProperty(Bound, 'prototype', {
    get() { prototypeReads++; return Base.prototype; }
  });
  class Lookup extends Bound {}
  expect(prototypeReads).toBe(1);
  expect(Object.getPrototypeOf(Lookup)).toBe(Bound);
  expect(Object.getPrototypeOf(Lookup.prototype)).toBe(Base.prototype);

  function throwsName(f, name) {
    try { f(); } catch (e) { expect(e.name).toBe(name); return; }
    throw 'expected class definition to throw';
  }
  function InvalidPrototype() {}
  InvalidPrototype.prototype = 2;
  function InvalidHeritage(base) { return class extends base { [(order += '!', 'bad')]() {} }; }
  throwsName(() => InvalidHeritage(5), 'TypeError');
  throwsName(() => InvalidHeritage(() => {}), 'TypeError');
  throwsName(() => InvalidHeritage(InvalidPrototype), 'TypeError');
  expect(order).toBe('HKS');
  throwsName(() => { class Self extends Self {} }, 'ReferenceError');
  throwsName(() => { class Self { [Self]() {} } }, 'ReferenceError');
  throwsName(() => { class Self { [Self] = 2; } }, 'ReferenceError');
  let conversions = 0;
  const badKey = { toString() { conversions++; return 'prototype'; } };
  throwsName(() => { class Bad { static [badKey]() {} static { order += '!'; } } }, 'TypeError');
  throwsName(() => { class Bad { static [badKey] = (order += '!'); } }, 'TypeError');
  expect(conversions).toBe(2);
  expect(order).toBe('HKS');

  function Factory(first) {
    this.key = 'method';
    return class Context {
      [this.key]() { return Context; }
      [arguments[0]] = () => new.target;
      [new.target.name] = () => this;
      [Symbol.for('named')]() {}
      static owner = () => this;
      static target = new.target;
    };
  }
  const Context = new Factory('target');
  const context = new Context();
  expect(context.method()).toBe(Context);
  expect(context.target()).toBe(undefined);
  expect(context.Factory()).toBe(context);
  expect(context[Symbol.for('named')].name).toBe('[named]');
  expect(Context.owner()).toBe(Context);
  expect(Context.target).toBe(undefined);
}

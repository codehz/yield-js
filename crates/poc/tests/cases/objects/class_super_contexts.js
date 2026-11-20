// @returns undefined

import { expect } from 'test';

export function run() {
  class Base {
    get receiver() { return this; }
    static get receiver() { return this; }
    method() { return this.n; }
  }
  class Derived extends Base {
    field = (() => super.receiver)();
    static field = super.receiver;
    static { this.arrow = () => super.receiver; }
    constructor() {
      const read = () => super.receiver;
      let early;
      try { read(); } catch (error) { early = error.name; }
      expect(early).toBe('ReferenceError');
      super();
      expect(read()).toBe(this);
    }
    method() { return (super.method)?.(); }
    get receiver() { return super.receiver; }
    static method() { return super.receiver; }
    missing() { return super.missing?.(); }
  }
  const instance = new Derived();
  instance.n = 3;
  expect(instance.field).toBe(instance);
  expect(instance.method()).toBe(3);
  expect(instance.receiver).toBe(instance);
  expect(instance.missing()).toBe(undefined);
  expect(Derived.field).toBe(Derived);
  expect(Derived.arrow()).toBe(Derived);
  const borrowed = { n: 6, method: Derived.prototype.method, staticMethod: Derived.method };
  expect(borrowed.method()).toBe(6);
  expect(borrowed.staticMethod()).toBe(borrowed);
  Object.setPrototypeOf(Derived.prototype, { method() { return this.n + 10; } });
  expect(borrowed.method()).toBe(16);
  Object.setPrototypeOf(Derived, { get receiver() { return this.n; } });
  expect(borrowed.staticMethod()).toBe(6);

  class Root { constructor() { expect(super.toString()).toBe('[object Object]'); } }
  new Root();
}

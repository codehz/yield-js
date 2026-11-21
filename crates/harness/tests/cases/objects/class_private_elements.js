// @returns undefined

import { expect } from 'test';

export function run() {
  function factory() {
    return class {
      #x = 4;
      #empty;
      #m() { return this.#x; }
      get #value() { return this.#x; }
      set #value(v) { this.#x = v; }
      static #s = 10;
      static get #accessor() { return this.#s; }
      static set #accessor(v) { this.#s = v; }
      static read(o) { return o.#x; }
      static has(o) { return #x in o; }
      set #writeOnly(v) { this.#x = v; }
      get #readOnly() { return this.#x; }
      readWriteOnly() { return this.#writeOnly; }
      writeReadOnly() { this.#readOnly = 0; }
      writeMethod() { this.#m = 0; }
      static readStatic() { return this.#accessor; }
      static writeStatic(v) { this.#accessor = v; }
      static call(o) { return o?.#m?.(); }
      static readOptional(o) { return o?.#x; }
      empty() { return this.#empty; }
      update() {
        const old = this.#value++;
        this.#value += 2;
        this.#value &&= 9;
        [this.#value] = [this.#value + 2];
        return old;
      }
      nested() {
        const outer = this;
        function captured() { return outer.#x; }
        const fn = function () { return outer.#x; };
        class Inner extends (#x in outer ? Object : null) {
          #x = 8;
          read() { return this.#x; }
          has(o) { return #x in o; }
          readOuter() { return outer.#m(); }
        }
        const inner = new Inner();
        expect(inner.read()).toBe(8);
        expect(inner.has(outer)).toBe(false);
        expect(inner.readOuter()).toBe(11);
        return captured() + fn();
      }
    };
  }
  function typeError(fn) {
    let name;
    try { fn(); } catch (error) { name = error.name; }
    expect(name).toBe('TypeError');
  }
  const A = factory();
  const B = factory();
  const a = new A();
  const b = new B();
  expect(A.read(a)).toBe(4);
  expect(A.has(a)).toBe(true);
  expect(A.has(b)).toBe(false);
  expect(A.has(Object.create(a))).toBe(false);
  expect(A.has(new Proxy(a, {}))).toBe(false);
  expect(Reflect.ownKeys(a).length).toBe(0);
  typeError(() => A.read(b));
  typeError(() => A.has(4));
  typeError(() => a.readWriteOnly());
  typeError(() => a.writeReadOnly());
  typeError(() => a.writeMethod());
  expect(a.empty()).toBe(undefined);
  expect(a.update()).toBe(4);
  expect(A.call(a)).toBe(11);
  expect(A.call(null)).toBe(undefined);
  expect(A.readOptional(undefined)).toBe(undefined);
  expect(a.nested()).toBe(22);
  A.writeStatic(12);
  expect(A.readStatic()).toBe(12);
  expect(B.readStatic()).toBe(10);
  class Sub extends A {}
  typeError(() => Sub.readStatic());

  // The hidden brand must already be initialized during computed keys.
  const computed = class {
    #x;
    [#x in {} ? 'wrong' : 'right']() { return 1; }
  };
  expect(new computed().right()).toBe(1);

  // Methods/accessors are installed before fields; fields appear in order.
  class Ordered {
    before = #later in this;
    value = this.#method();
    #later = 6;
    after = #later in this;
    #method() { return 5; }
    static before = #end in this;
    static value = this.#methodStatic();
    static #end;
    static after = #end in this;
    static #methodStatic() { return 7; }
  }
  const ordered = new Ordered();
  expect(ordered.before).toBe(false);
  expect(ordered.after).toBe(true);
  expect(ordered.value).toBe(5);
  expect(Ordered.before).toBe(false);
  expect(Ordered.after).toBe(true);
  expect(Ordered.value).toBe(7);

  const replacement = {};
  class Base { constructor() { return replacement; } }
  class Stamp extends Base {
    #stamp = 3;
    static has(o) { return #stamp in o; }
  }
  expect(new Stamp()).toBe(replacement);
  expect(Stamp.has(replacement)).toBe(true);
  typeError(() => new Stamp());
}

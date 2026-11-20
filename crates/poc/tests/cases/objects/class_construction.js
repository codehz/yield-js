// @returns undefined

import { expect } from 'test';

export function run() {
  function throwsName(f, name) {
    try { f(); } catch (e) { expect(e.name).toBe(name); return; }
    throw 'expected construction to throw';
  }

  class BaseFields {
    field = 42;
    constructor(value = this.field) { this.fromDefault = value; }
  }
  expect(new BaseFields().fromDefault).toBe(42);

  let baseCalls = 0;
  let fieldCalls = 0;
  class Base {
    constructor(a, b, c) {
      baseCalls++;
      this.sum = a + b;
      this.last = c;
      this.target = new.target;
    }
  }
  class Early extends Base {
    constructor(value = this) { super(value, 0); }
  }
  throwsName(() => new Early(), 'ReferenceError');
  expect(baseCalls).toBe(0);

  class Duplicate extends Base {
    field = ++fieldCalls;
    constructor() { super(1, 2); super(3, 4); }
  }
  throwsName(() => new Duplicate(), 'ReferenceError');
  expect(baseCalls).toBe(2);
  expect(fieldCalls).toBe(1);

  const replacement = {};
  class ReplacingBase { constructor() { return replacement; } }
  class OnReplacement extends ReplacingBase {
    field = (() => this)();
    constructor() { expect(super()).toBe(replacement); }
  }
  expect(new OnReplacement()).toBe(replacement);
  expect(replacement.field).toBe(replacement);

  class FinallyBinds extends Base {
    constructor() { try { return; } finally { super(4, 5); } }
  }
  expect(new FinallyBinds().sum).toBe(9);
  class FinallyReplaces extends Base {
    constructor() { try { return null; } finally { return replacement; } }
  }
  expect(new FinallyReplaces()).toBe(replacement);
  class FinallyReturnsPrimitive extends Base {
    constructor() { try { super(1, 2); } finally { return null; } }
  }
  throwsName(() => new FinallyReturnsPrimitive(), 'TypeError');

  class Lexical extends Base {
    constructor(...args) {
      const read = () => () => this;
      const readTarget = () => new.target;
      const invoke = () => super(1, ...args, 9);
      function ownThis() { return (() => this)(); }
      expect(ownThis.call(replacement)).toBe(replacement);
      const before = read();
      throwsName(before, 'ReferenceError');
      expect(invoke()).toBe(before());
      this.read = before;
      this.readTarget = readTarget;
      class Nested {
        field = () => this;
        static { this.owner = () => this; }
      }
      const nested = new Nested();
      expect(nested.field()).toBe(nested);
      expect(Nested.owner()).toBe(Nested);
    }
  }
  function Alternate() {}
  const lexical = Reflect.construct(Lexical, [6], Alternate);
  expect(lexical.sum).toBe(7);
  expect(lexical.last).toBe(9);
  expect(lexical.read()).toBe(lexical);
  expect(lexical.readTarget()).toBe(Alternate);
  expect(lexical.target).toBe(Alternate);
  expect(Object.getPrototypeOf(lexical)).toBe(Alternate.prototype);
  throwsName(() => Lexical(6), 'TypeError');

  class NullDefault extends null {}
  class NullSuper extends null { constructor() { super(); } }
  class NullMissingThis extends null { constructor() {} }
  class NullReturning extends null { constructor() { return replacement; } }
  throwsName(() => new NullDefault(), 'TypeError');
  throwsName(() => new NullSuper(), 'TypeError');
  throwsName(() => new NullMissingThis(), 'ReferenceError');
  expect(new NullReturning()).toBe(replacement);

  class Forward extends Base {}
  class ForwardAgain extends Forward {}
  const iterator = Array.prototype[Symbol.iterator];
  let forwarded;
  try {
    Array.prototype[Symbol.iterator] = function () { throw 'default constructor used iterator'; };
    forwarded = new ForwardAgain(10, 20);
  } finally {
    Array.prototype[Symbol.iterator] = iterator;
  }
  expect(forwarded.sum).toBe(30);
  expect(forwarded.target).toBe(ForwardAgain);
  expect(forwarded instanceof ForwardAgain).toBe(true);
}

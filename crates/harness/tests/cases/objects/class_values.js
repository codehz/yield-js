// @pending ["wait:class"]
// @restore
// @fulfill ["wait:class",0]
// @fulfilled undefined

import { expect, wait } from 'test';

export class Counter {
  constructor(value = 1) {
    this.value = value;
    this.target = new.target;
    this.owner = Counter;
  }
  get n() { return this.value; }
  set n(value) { this.value = value; }
  'double'() { return this.n * 2; }
  42() { return this.n; }
  static make(value) { return new Counter(value); }
  static get type() { return Counter; }
  static set type(value) { this.extra = value; }
  static async afterWait() {
    await wait('class');
    return Counter;
  }
  static writeInner(value) { Counter = value; }
}

function factory(label) {
  return class Local {
    static label() { return label; }
    static who() { return () => Local; }
    static nested() {
      const result = class Nested {
        static outer() { return Local; }
        static inner() { return Nested; }
      };
      expect(Local).toBe(this);
      return result;
    }
  };
}

function identity(value) { return value; }

export async function run() {
  const original = identity(Counter);
  const Box = (class {});
  expect(Box.name).toBe('Box');
  expect((class {}).name).toBe('');
  expect(new Box() instanceof Box).toBe(true);
  expect(original.name).toBe('Counter');
  expect(original.length).toBe(0);
  const item = original.make(4);
  expect(item instanceof original).toBe(true);
  expect(item.constructor).toBe(original);
  expect(item.target).toBe(original);
  item.n = 6;
  expect(item.double()).toBe(12);
  expect(item[42]()).toBe(6);
  expect(Object.keys(original.prototype).length).toBe(0);
  expect(Object.keys(original).length).toBe(0);
  original.type = 9;
  expect(original.extra).toBe(9);
  expect(original.type).toBe(original);
  try { original(); throw 'class call succeeded'; }
  catch (error) { expect(error.name).toBe('TypeError'); }
  try { new item.double(); throw 'method construction succeeded'; }
  catch (error) { expect(error.name).toBe('TypeError'); }
  try { original.writeInner(Box); throw 'inner assignment succeeded'; }
  catch (error) { expect(error.name).toBe('TypeError'); }
  Counter = Box;
  expect(Counter).toBe(Box);
  expect(new original().owner).toBe(original);
  expect(original.type).toBe(original);

  const a = factory('a');
  const b = factory('b');
  expect(a.label()).toBe('a');
  expect(b.label()).toBe('b');
  expect(a === b).toBe(false);
  expect(a.who()()).toBe(a);
  expect(b.who()()).toBe(b);
  const nested = a.nested();
  expect(nested.outer()).toBe(a);
  expect(nested.inner()).toBe(nested);

  const classes = [];
  for (let i = 0; i < 2; i++) {
    class Iteration {
      static who() { return Iteration; }
    }
    classes[i] = Iteration;
    Iteration = Box;
    expect(Iteration).toBe(Box);
  }
  expect(classes[0] === classes[1]).toBe(false);
  expect(classes[0].who()).toBe(classes[0]);
  expect(classes[1].who()).toBe(classes[1]);

  expect(await original.afterWait()).toBe(original);
  expect(a.who()()).toBe(a);
  expect(nested.outer()).toBe(a);
  expect(nested.inner()).toBe(nested);
  expect(classes[0].who()).toBe(classes[0]);
  expect(classes[1].who()).toBe(classes[1]);
}

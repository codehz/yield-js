// @returns 42

import { expect, gc } from 'test';

function nested(value) {
  gc();
  return value.n;
}

export function run() {
  let collections = 0;
  class Target {
    field = (gc(), collections++, 1);
    constructor(object, key, ...rest) {
      expect(arguments[0]).toBe(object);
      expect(arguments[1]).toBe(key);
      expect(rest[0]).toBe(20);
      object[key] = nested(object);
      this.result = object[key] + rest[0] + this.field;
    }
  }
  const alternate = (function () {}).bind(null);
  const parent = {
    get prototype() { gc(); collections++; return Target.prototype; },
  };
  Object.setPrototypeOf(alternate, parent);
  // Neither fresh value has a persistent owner while Reflect.construct
  // forwards its arguments through the getter and the field initializer.
  const instance = Reflect.construct(Target, {
    length: 3,
    get 0() { return { n: 21 }; },
    get 1() { return Symbol('argument'); },
    2: 20,
  }, alternate);
  expect(collections).toBe(2);
  return instance.result;
}

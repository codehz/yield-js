// @pending ["wait:properties"]
// @restore
// @fulfill ["wait:properties", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

function Descriptors() {}

export async function run() {
  const define = Object.defineProperties;
  const target = {};
  const symbol = Symbol("property");
  let trace = "";
  Descriptors.prototype = { get inherited() { throw "inherited"; } };
  const source = new Descriptors();
  source.z = { get value() { trace += "z"; return 30; } };
  source[10] = { get value() { trace += "a"; return 10; } };
  source.skip = null;
  Object.defineProperty(source, "hidden", {
    configurable: true,
    get: function () { trace += "h"; return { value: 40 }; },
  });
  Object.defineProperty(source, 2, {
    enumerable: true,
    get: function () {
      trace += "2";
      Object.defineProperty(this, "skip", { enumerable: false });
      Object.defineProperty(this, "hidden", { enumerable: true });
      this.added = null;
      return { value: 20 };
    },
  });
  source[symbol] = {
    get get() {
      trace += "s";
      if ("z" in target) throw "applied before conversion finished";
      return function () { return this.z + this[2]; };
    },
  };
  expect(define(target, source)).toBe(target);
  expect(trace).toBe("2azhs");
  await wait("properties");
  const keys = Reflect.ownKeys(target);
  expect(keys.length).toBe(5);
  expect(keys[0]).toBe("2");
  expect(keys[1]).toBe("10");
  expect(keys[2]).toBe("z");
  expect(keys[3]).toBe("hidden");
  expect(keys[4]).toBe(symbol);
  expect(target.hidden).toBe(40);
  expect(target[symbol]).toBe(50);
}

// @fulfilled undefined

import { expect, gc } from 'test';

export async function run() {
  const join = [].join;
  const seen = [];
  Object.defineProperty(String.prototype, "2", {
    configurable: true,
    get() { throw new Error("join must not read past the string's own length"); },
  });
  expect(Reflect.apply(join, "hi", [","])).toBe("h,i");
  expect(Reflect.apply(join, "😀", [":"])).toBe("\ud83d:\ude00");
  expect(Reflect.apply(join, "😀", [""])).toBe("😀");
  delete String.prototype[2];
  expect(Reflect.apply(Array.prototype.toString, true, [])).toBe("[object Boolean]");
  Object.defineProperty(Boolean.prototype, "join", {
    configurable: true,
    get() { const box = this; return function () { return this === box; }; },
  });
  expect(Reflect.apply(Array.prototype.toString, true, [])).toBe(true);
  delete Boolean.prototype.join;
  Object.defineProperty(Number.prototype, "length", {
    configurable: true,
    get() { this.marker = "number"; return 1; },
  });
  Object.defineProperty(Number.prototype, "0", {
    configurable: true,
    get() { seen.push(this); return this.marker; },
  });
  const separator = {
    toString() {
      gc();
      expect(seen.length).toBe(0);
      return ":";
    },
  };
  expect(Reflect.apply(join, 7, [separator])).toBe("number");
  expect(seen.length).toBe(1);
  expect(seen[0] instanceof Number).toBe(true);
  expect(Number.prototype.valueOf.call(seen[0])).toBe(7);
  expect(Array.from(8)[0]).toBe("number");
  expect(Number.prototype.valueOf.call(seen[1])).toBe(8);
  Object.defineProperty(Number.prototype, Symbol.iterator, {
    configurable: true,
    value() { return [this][Symbol.iterator](); },
  });
  expect(Array.from(9)[0]).toBe(9);
  delete Number.prototype[Symbol.iterator];
  delete Number.prototype.length;
  delete Number.prototype[0];
}

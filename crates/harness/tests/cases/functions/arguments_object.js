// @returns undefined

import { expect } from 'test';

export function run() {
  const obj = {
    read(a, ...rest) {
      let sum = 0;
      for (const value of arguments) sum += value;
      const length = Object.getOwnPropertyDescriptor(arguments, "length");
      return this === obj && a === 2 && rest.length === 2 && rest[0] === 3
        && arguments.length === 3 && arguments[2] === 5 && sum === 10
        && Object.getPrototypeOf(arguments) === Object.prototype
        && !Array.isArray(arguments)
        && arguments[Symbol.iterator] === Array.prototype.values
        && length.writable && length.configurable && !length.enumerable
        && arguments.callee === undefined && arguments.caller === undefined;
    }
  };
  expect(obj.read(2, 3, 5)).toBe(true);
}

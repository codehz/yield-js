// @returns undefined

import { expect } from 'test';

export function run() {
  expect(BigInt()).toBe(0n);
  expect(BigInt(true)).toBe(1n);
  expect(BigInt(false)).toBe(0n);
  expect(BigInt(1)).toBe(1n);
  expect(BigInt(-0)).toBe(0n);
  expect(BigInt(2 ** 60)).toBe(1152921504606846976n);
  expect(BigInt('')).toBe(0n);
  expect(BigInt('   ')).toBe(0n);
  expect(BigInt(' 0x10 ')).toBe(16n);
  expect(BigInt('-0b11')).toBe(-3n);
  expect(BigInt('+0o17')).toBe(15n);
  expect(BigInt('0123')).toBe(123n);
  expect(BigInt(1n)).toBe(1n);

  expect(Number(1n)).toBe(1);
  expect(Number(-1n)).toBe(-1);
  expect(Number(2n ** 1024n)).toBe(Infinity);
  expect(String(-123n)).toBe('-123');
  expect(`${2n ** 64n}`).toBe('18446744073709551616');
  expect(Boolean(0n)).toBe(false);
  expect(Boolean(-1n)).toBe(true);
  expect(Number.parseInt(123n)).toBe(123);
  expect(Number.parseFloat(1n)).toBe(1);

  expect(() => BigInt(1.5)).toThrow('not an integer');
  expect(() => BigInt(NaN)).toThrow('not an integer');
  expect(() => BigInt(Infinity)).toThrow('not an integer');
  expect(() => BigInt('1.5')).toThrow('Cannot convert string');
  expect(() => BigInt('0x')).toThrow('Cannot convert string');
  expect(() => BigInt(null)).toThrow('null');
  expect(() => BigInt(Symbol())).toThrow('Symbol');
  expect(() => BigInt(undefined)).toThrow('undefined');
  expect(() => Math.abs(1n)).toThrow('BigInt');
  expect(() => Number.prototype.toString.call(1n)).toThrow('incompatible');
  expect(() => JSON.stringify(1n)).toThrow('BigInt');
  expect(() => JSON.stringify({value: 1n})).toThrow('BigInt');
  expect(JSON.stringify(1n, (key, value) => typeof value === 'bigint' ? value.toString() : value)).toBe('"1"');
  expect(BigInt({ valueOf() { return 7n; } })).toBe(7n);
  expect(BigInt({ toString() { return '8'; } })).toBe(8n);
  expect(() => new BigInt(1)).toThrow('not a constructor');

  expect(BigInt.prototype.constructor).toBe(BigInt);
  expect(Object.prototype.toString.call(Object(1n))).toBe('[object BigInt]');
  expect(Object(1n) instanceof BigInt).toBe(true);
  expect((123n).valueOf()).toBe(123n);
  expect(globalThis.BigInt).toBe(BigInt);
  expect(Object.getPrototypeOf(1n)).toBe(BigInt.prototype);

  const keyed = { 1n: 2, [2n]: 3 };
  expect(keyed[1n]).toBe(2);
  expect(keyed['2']).toBe(3);
  expect(Object.keys(keyed).join()).toBe('1,2');
}

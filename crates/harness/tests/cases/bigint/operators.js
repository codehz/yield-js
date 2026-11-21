// @returns undefined

import { expect } from 'test';

export function run() {
  expect(typeof 1n).toBe('bigint');
  expect(0x10n).toBe(16n);
  expect(0b101n).toBe(5n);
  expect(0o17n).toBe(15n);
  expect(1_000n).toBe(1000n);
  expect(-5n).toBe(-5n);
  expect(~0n).toBe(-1n);
  expect(5n & 3n).toBe(1n);
  expect(5n | 3n).toBe(7n);
  expect(5n ^ 3n).toBe(6n);
  expect(1n << 65n).toBe(36893488147419103232n);
  expect(1n << -1n).toBe(0n);
  expect(16n >> 2n).toBe(4n);
  expect(-1n >> 1n).toBe(-1n);
  expect(1n >> -2n).toBe(4n);
  expect(7n / 3n).toBe(2n);
  expect(-7n / 3n).toBe(-2n);
  expect(7n / -3n).toBe(-2n);
  expect(7n % 3n).toBe(1n);
  expect(-7n % 3n).toBe(-1n);
  expect(2n ** 100n).toBe(1267650600228229401496703205376n);
  expect(1n + 'x').toBe('1x');
  expect('x' + 1n).toBe('x1');
  expect(String(1n + 2n)).toBe('3');

  expect(1n < 2).toBe(true);
  expect(1n <= 1).toBe(true);
  expect(2n > 1.5).toBe(true);
  expect(1n == 1).toBe(true);
  expect(0n == -0).toBe(true);
  expect(1n == '1').toBe(true);
  expect(1n == true).toBe(true);
  expect(0n == false).toBe(true);
  expect(1n === 1).toBe(false);
  expect(1n !== 1).toBe(true);
  expect(1n == 1.5).toBe(false);
  expect(1n < 1.5).toBe(true);
  expect(-2n < -1.5).toBe(true);

  expect(() => 1n + 1).toThrow('Cannot mix BigInt');
  expect(() => 1n - 1).toThrow('Cannot mix BigInt');
  expect(() => 1n & 1).toThrow('Cannot mix BigInt');
  expect(() => 1n >>> 1n).toThrow('unsigned right shift');
  expect(() => +1n).toThrow('BigInt');
  expect(() => 1n ** -1n).toThrow('non-negative');
  expect(() => 1n / 0n).toThrow('Division by zero');
  expect(() => 1n % 0n).toThrow('Division by zero');

  expect(BigInt.asIntN(8, 255n)).toBe(-1n);
  expect(BigInt.asUintN(8, -1n)).toBe(255n);
  expect(BigInt.asIntN(0, 123n)).toBe(0n);
  expect(BigInt.asUintN(64, 1n << 64n)).toBe(0n);
  expect(BigInt.asIntN(1, 1n)).toBe(-1n);
  expect(BigInt.asUintN(1, -1n)).toBe(1n);
  expect(BigInt.asIntN(9007199254740991, 1n)).toBe(1n);
  expect(BigInt.asUintN(9007199254740991, 1n)).toBe(1n);
  expect((255n).toString(16)).toBe('ff');
  expect((-255n).toString(16)).toBe('-ff');
  expect((255n).toLocaleString()).toBe('255');

  let x = 1n;
  expect(x++).toBe(1n);
  expect(x).toBe(2n);
  expect(++x).toBe(3n);
  expect(x--).toBe(3n);
  expect(x).toBe(2n);
  expect(--x).toBe(1n);
  const holder = { n: 1n };
  expect(holder.n++).toBe(1n);
  expect(holder.n).toBe(2n);
  expect(++holder.n).toBe(3n);
  expect(holder.n--).toBe(3n);
  expect(holder.n).toBe(2n);
  expect(--holder.n).toBe(1n);
  let boxed = { valueOf() { return 10n; } };
  expect(boxed++).toBe(10n);
  expect(boxed).toBe(11n);

  const spread = { ...{ a: 1n } };
  expect(spread.a).toBe(1n);
  const assigned = Object.assign({}, { b: 2n });
  expect(assigned.b).toBe(2n);
  let caught;
  try {
    throw 2n;
  } catch (e) {
    caught = e;
  }
  expect(caught).toBe(2n);
  let switched = '';
  switch (2n) {
    case 1n: switched = 'one'; break;
    case 2n: switched = 'two'; break;
    default: switched = 'other';
  }
  expect(switched).toBe('two');

  expect(Object.prototype.toString.call(1n)).toBe('[object BigInt]');
  expect(typeof Object(1n)).toBe('object');
  expect(Object(1n).valueOf()).toBe(1n);
  expect(new Object(1n).valueOf()).toBe(1n);
}

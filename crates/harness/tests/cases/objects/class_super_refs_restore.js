// @pending ["wait:super reference"]
// @restore
// @fulfill ["wait:super reference",4]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  let order = '';
  const key = { toString() { order += 'K'; return 'x'; } };
  const original = {
    get x() { return this.n; },
    set x(value) { this.n = value; }
  };
  const next = {
    get x() { return this.n * 2; },
    set x(value) { this.n = -value; }
  };
  const home = {
    get value() { return super.x; },
    set value(value) { super.x = value; },
    async update() {
      const value = super[key] += (Object.setPrototypeOf(home, next), await wait('super reference'));
      expect(value).toBe(7);
      expect(this.n).toBe(7);
      expect(super.x).toBe(14);
    },
    updateAgain() {
      expect(super[key]++).toBe(14);
      expect(this.n).toBe(-15);
      expect(super[key] ||= 99).toBe(-30);
      expect(super[key] &&= 5).toBe(5);
      expect(this.n).toBe(-5);
      [super[key]] = [8];
      ({ x: super[key] } = { x: 9 });
      expect(this.n).toBe(-9);
      super[key] = (order += 'R', 10);
      expect(this.n).toBe(-10);
      let failure;
      try { delete super[(order += 'D', key)]; } catch (error) { failure = error.name; }
      expect(failure).toBe('ReferenceError');
    },
    read() { return super.x?.value; }
  };
  Object.setPrototypeOf(home, original);
  home.value = 31;
  expect(home.value).toBe(31);
  const receiver = { n: 3, update: home.update, updateAgain: home.updateAgain, read: home.read };
  await receiver.update();
  receiver.updateAgain();
  expect(order).toBe('KKKKKKRKD');
  expect(home.n).toBe(31);
  Object.setPrototypeOf(home, null);
  let failure;
  try { receiver.read(); } catch (error) { failure = error.name; }
  expect(failure).toBe('TypeError');
}

// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled 7

import { expect, wait } from 'test';

export async function run() {
  let initialized = 0;
  class Base { m() { return 3; } }
  class Derived extends Base {
    #x = 4;
    static { initialized++; }
    async m() {
      expect(this.#x).toBe(4);
      await wait('a');
      expect(initialized).toBe(1);
      return super.m() + this.#x;
    }
  }
  return await new Derived().m();
}

// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const sparse = [1, , null, undefined, 5];
  expect(sparse.join("|")).toBe("1||||5");
  expect(`${sparse}`).toBe("1,,,,5");
  expect(+[7]).toBe(7);
  expect([7] + 1).toBe("71");

  const cyclic = [1];
  cyclic[1] = cyclic;
  cyclic[2] = 3;
  expect(cyclic.join("|")).toBe("1||3");
  expect(`${cyclic}`).toBe("1,,3");

  let reads = 0;
  const withGetter = [0, 2];
  Object.defineProperty(withGetter, "0", {
    get() {
      reads++;
      this[1] = 8;
      this[2] = 9;
      return 7;
    }
  });
  expect(`${withGetter}`).toBe("7,8");
  expect(reads).toBe(1);

  const custom = [4];
  let calls = "";
  custom.valueOf = function () { calls += "v"; return this[0]; };
  custom.join = function () { calls += "j"; return "joined:" + this[0]; };
  expect(+custom).toBe(4);
  expect(custom + 1).toBe(5);
  expect(`${custom}`).toBe("joined:4");
  custom.toString = function () { calls += "s"; return "overridden"; };
  expect(`${custom}`).toBe("overridden");
  expect(calls).toBe("vvjs");
}

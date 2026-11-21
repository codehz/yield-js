// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const a = [0, 1, 2, 3];
  let calls = 0;
  const len = { valueOf() { calls++; return "2"; } };
  a.length = len;
  expect(calls).toBe(2);
  expect(a.length).toBe(2);
  expect(2 in a).toBe(false);
  Object.defineProperties(a, { length: { value: len } });
  expect(calls).toBe(4);
  expect(a.length).toBe(2);
  expect(Reflect.set(a, "length", len)).toBe(true);
  expect(calls).toBe(6);

  const different = { valueOf() { calls++; return calls === 7 ? 1 : 2; } };
  let differentError;
  try { Reflect.defineProperty(a, "length", { value: different }); } catch (e) { differentError = e; }
  expect(differentError?.name).toBe("RangeError");
  expect(calls).toBe(8);
  expect(a.length).toBe(2);

  const marker = {};
  const throwing = {
    [Symbol.toPrimitive](hint) {
      if (hint !== "number") throw "wrong hint";
      calls++;
      if (calls === 10) throw marker;
      return 1;
    }
  };
  let primitiveError;
  try { Object.defineProperty(a, "length", { value: throwing }); } catch (e) { primitiveError = e; }
  expect(primitiveError).toBe(marker);
  expect(calls).toBe(10);
  expect(a.length).toBe(2);
  let symbolError;
  try { a.length = Symbol(); } catch (e) { symbolError = e; }
  expect(symbolError?.name).toBe("TypeError");
}

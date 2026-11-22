// @returns true

import { expect } from "test";

export function run() {
  let local = 1;
  const indirect = eval;
  let coercions = 0;
  const input = { [Symbol.toPrimitive]() { ++coercions; throw "coerced"; } };
  for (const attempt of [
    () => eval("local = 2"), () => eval("not valid JS!"), () => eval(),
    () => eval(42), () => eval(input), () => indirect(Symbol()),
    () => globalThis.eval("globalThis.unexpected = true"),
  ]) {
    try { attempt(); return "eval returned"; }
    catch (error) { expect(error instanceof EvalError).toBe(true); }
  }
  try { new indirect(input); return "eval constructed"; }
  catch (error) { expect(error instanceof TypeError).toBe(true); }
  return local === 1 && coercions === 0 && globalThis.unexpected === undefined;
}

// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let order = "";
  function key(mark) { order += mark; return "x"; }
  function rhs() { order += "r"; return 2; }
  let reads = 0, writes = 0;
  const pair = {
    n: 5,
    get [key("g")]() { throw "overwritten getter"; },
    set [key("s")](v) { writes = -10; },
    get x() { reads += 1; return this.n; },
    set x(v) { writes += 1; this.n = v; return 999; }
  };
  const result = (pair[key("c")] += rhs());
  expect(order).toBe("gscr");
  expect(result).toBe(7);
  expect(pair.n).toBe(7);
  expect(reads).toBe(1);
  expect(writes).toBe(1);

  const data = {set x(v) { throw "old setter"; }, [key("d")]: rhs()};
  expect(data.x).toBe(2);
  expect(order).toBe("gscrdr");
  const setterOnly = {get x() { return 99; }, x: 3, set x(v) { this.n = v; }};
  expect(setterOnly.x).toBe(undefined);
  expect((setterOnly.x = 8)).toBe(8);
  expect(setterOnly.n).toBe(8);

  function C() {}
  C.prototype = {get x() { return this.n; }};
  const o = new C();
  o.n = 6;
  let assignmentError;
  try {
    o.x = 0;
  } catch (e) {
    assignmentError = e;
  }
  expect(typeof assignmentError).toBe("object");
  expect(assignmentError?.name).toBe("TypeError");
  expect(assignmentError.message).toBe("Cannot set property without a setter");
  expect(o.x).toBe(6);
  const throws = {set x(v) { throw v; }};
  let setterError;
  try { throws.x = 4; } catch (e) { setterError = e; }
  expect(setterError).toBe(4);
}

// @pending ["wait:rhs"]
// @restore
// @fulfill ["wait:rhs",0]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const symbol = Symbol("x");
  function Box() {}
  Box.prototype = {
    [symbol]: undefined,
    get inherited() { throw "presence must not read a getter"; },
  };
  const box = new Box();
  box.own = undefined;
  expect("own" in box).toBe(true);
  expect("inherited" in box).toBe(true);
  expect("missing" in box).toBe(false);
  expect(box.hasOwnProperty("inherited")).toBe(false);
  expect("call" in Box).toBe(true);
  expect("hasOwnProperty" in box).toBe(true);
  const array = [undefined];
  expect(0 in array).toBe(true);
  expect(1 in array).toBe(false);
  expect("length" in array).toBe(true);
  expect(Symbol.iterator in array).toBe(true);
  expect(Symbol("x") in box).toBe(false);
  expect("x" in box).toBe(false);

  let order = "";
  function left() { order += "L"; return symbol; }
  function right() { order += "R"; return box; }
  expect(left() in (await wait("rhs"), right())).toBe(true);
  expect(order).toBe("LR");

  let error;
  try {
    "x" in 1;
  } catch (caught) {
    error = caught;
  }
  expect(error?.name).toBe("TypeError");
  expect(order).toBe("LR");
}

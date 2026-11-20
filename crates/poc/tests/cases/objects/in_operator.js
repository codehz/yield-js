// @pending ["wait:rhs"]
// @restore
// @fulfill ["wait:rhs",0]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const symbol = Symbol("x");
  function Box() {}
  Box.prototype = {
    [symbol]: undefined,
    get inherited() { throw "presence must not read a getter"; },
  };
  const box = new Box();
  box.own = undefined;
  if (!("own" in box) || !("inherited" in box) || "missing" in box) return false;
  if (box.hasOwnProperty("inherited") || !("call" in Box) || !("hasOwnProperty" in box)) return false;
  const array = [undefined];
  if (!(0 in array) || 1 in array || !("length" in array) || !(Symbol.iterator in array)) return false;
  if (Symbol("x") in box || "x" in box) return false;

  let order = "";
  function left() { order += "L"; return symbol; }
  function right() { order += "R"; return box; }
  if (!(left() in (await wait("rhs"), right())) || order !== "LR") return false;

  try {
    "x" in 1;
  } catch (error) {
    return error.name === "TypeError" && order === "LR";
  }
  return false;
}

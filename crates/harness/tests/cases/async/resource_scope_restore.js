// @pending ["wait:body"]
// @restore
// @fulfill ["wait:body", null]
// @pending ["wait:b"]
// @restore
// @reject ["wait:b", "B"]
// @fulfilled true

import { gc, wait } from "test";

export async function run() {
  let trace = "";
  async function work() {
    await using a = { [Symbol.dispose]() { gc(); trace += "A"; throw "A"; } };
    await using b = { [Symbol.asyncDispose]() { trace += "B"; return wait("b"); } };
    a[Symbol.dispose] = undefined;
    b[Symbol.asyncDispose] = undefined;
    gc();
    await wait("body");
    return "not returned";
  }
  try { await work(); }
  catch (e) {
    return trace === "BA" && e instanceof SuppressedError && e.error === "A" && e.suppressed === "B";
  }
  return false;
}

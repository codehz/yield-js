// @pending ["wait:dispose"]
// @restore
// @reject ["wait:dispose", "dispose"]
// @fulfilled true

import { wait } from "test";

export async function run() {
  let trace = "";
  const iterable = {
    [Symbol.iterator]() {
      return {
        next() { return { done: false, value: {
          [Symbol.asyncDispose]() { trace += "dispose/"; return wait("dispose"); },
        } }; },
        return() { trace += "close/"; throw "close"; },
      };
    },
  };
  async function work() {
    for (await using value of iterable) {
      trace += "body/";
      return 42;
    }
  }
  try { await work(); }
  catch (failure) { return failure === "dispose" && trace === "body/dispose/close/"; }
  return false;
}

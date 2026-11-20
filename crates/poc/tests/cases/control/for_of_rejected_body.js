// @pending ["wait:body"]
// @restore
// @reject ["wait:body", "original"]
// @fulfilled "INVRoriginal"

import { wait } from 'test';

export async function run() {
  let trace = "";
  const iterator = {
    get next() {
      trace += this === iterator ? "N" : "bad";
      return function () {
        return { get value() { trace += "V"; return 1; }, done: 0 };
      };
    },
    get return() {
      trace += this === iterator ? "R" : "bad";
      throw "replacement";
    }
  };
  const iterable = {
    [Symbol.iterator]() {
      trace += this === iterable ? "I" : "bad";
      return iterator;
    }
  };
  try {
    Symbol.iterator = null;
    return "Symbol.iterator accepted a write";
  } catch (error) {
    if (error.name !== "TypeError") throw error;
  }
  try {
    for (var value of iterable) {
      await wait("body");
    }
  } catch (error) {
    trace += error;
  }
  return trace;
}

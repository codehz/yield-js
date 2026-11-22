// @pending ["wait:value"]
// @restore
// @fulfill ["wait:value", 4]
// @pending ["wait:mapped"]
// @restore
// @fulfill ["wait:mapped", 6]
// @pending ["wait:done"]
// @restore
// @fulfill ["wait:done", 99]
// @fulfilled "2:3:11:0:1"

import { gc, wait } from "test";

export async function run() {
  let order = "";
  const source = {
    get [Symbol.asyncIterator]() { return null; },
    [Symbol.iterator]() {
      order += "i";
      let index = 0;
      return {
        get next() {
          order += "n";
          return function () {
            gc();
            index++;
            if (index === 1) return { done: false, value: 2 };
            if (index === 2) return { done: false, value: wait("value") };
            return { done: true, value: wait("done") };
          };
        }
      };
    }
  };
  function Result() {
    order += "c";
    this.arity = arguments.length;
  }
  const promise = Array.fromAsync.call(Result, source, async function (value, index) {
    gc();
    if (index === 1) value += await wait("mapped");
    return value + this.offset;
  }, { offset: 1 });
  if (order !== "inc") throw order;
  const result = await promise;
  return result.length + ":" + result[0] + ":" + result[1] + ":" + result.arity + ":" + (result instanceof Result ? 1 : 0);
}

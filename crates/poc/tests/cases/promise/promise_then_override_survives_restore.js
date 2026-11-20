// @pending ["wait:override"]
// @restore
// @fulfill ["wait:override", 10]
// @fulfilled "get;get;get;get;then;then;then;then;11,11,11,11"

import { wait } from 'test';

export async function run() {
  let trace = "";
  const gate = wait("override");
  const source = Promise.resolve(0);
  source.constructor = null;
  Object.defineProperty(source, "then", {
    configurable: true,
    get() {
      trace += "get;";
      return function(resolve, reject) {
        if (this !== source) throw "wrong then receiver";
        trace += "then;";
        gate.then(value => {
          resolve(value + 1);
          reject("ignored");
        });
      };
    }
  });
  const manual = new Promise(resolve => resolve(source));
  const wrapped = Promise.resolve(source);
  const consume = async () => await source;
  const forward = async () => source;
  const awaited = consume();
  const returned = forward();
  if (wrapped === source || trace !== "get;get;get;get;") throw trace;
  Object.defineProperty(source, "then", {value: null});
  if (await source !== source) throw "non-callable then must preserve the object";
  return trace + [await manual, await wrapped, await awaited, await returned].join(",");
}

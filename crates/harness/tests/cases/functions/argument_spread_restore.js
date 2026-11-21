// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 7]
// @pending ["wait:b7"]
// @restore
// @fulfill ["wait:b7", 8]
// @fulfilled "CISM|9:1:2:7:8:3"

import { wait } from 'test';

export async function run() {
  let trace = "";
  const obj = {
    id: 9,
    get method() {
      trace += "C";
      return function (a, b, c, d, e) {
        trace += "M";
        return `${this.id}:${a === b ? a.value : "lost identity"}:${b.extra}:${c}:${d}:${e}`;
      };
    },
  };
  const same = { value: 1, extra: 2 };
  const spread = {
    [Symbol.iterator]() {
      trace += "I";
      return [same].values();
    },
  };
  function suffix(value) {
    trace += "S";
    return value;
  }
  const result = obj.method(same, ...spread, await wait(...["a"]), await wait(...["b" + suffix(7)]), ...[3]);
  return `${trace}|${result}`;
}

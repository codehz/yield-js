// @pending ["wait:string-prototype"]
// @restore
// @fulfill ["wait:string-prototype", null]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const text = String.fromCodePoint(233, 128512, 122);
  const proto = String.prototype;
  const iteratorMethod = proto[Symbol.iterator];
  const iterator = text[Symbol.iterator]();
  const first = iterator.next();
  proto.readLength = function () { return this.length; };
  proto[Symbol.iterator] = function () { return this; };
  await wait("string-prototype");
  return first.value === "é" && !first.done && typeof text === "string"
    && String.prototype === proto && text.readLength() === 4 && text.missing === undefined
    && text[Symbol.iterator]() === text && proto[Symbol.iterator] !== iteratorMethod
    && iterator.next().value === "😀" && iterator.next().value === "z" && iterator.next().done
    && text.slice(1, 3) === "😀" && String.fromCharCode(122) === "z";
}

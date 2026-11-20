// @args [{"utf16":[55296]}]
// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", {"utf16":[56320]}]
// @fulfilled {"utf16":[55296,65533,56320]}

import { wait } from 'test';

export async function run(input) {
  const key = "\uD800";
  if (key !== String.fromCharCode(55296) || key !== input) throw "literal or host argument";
  const obj = { "\uD800": key, "\uFFFD": "\uFFFD" };
  const tail = await wait("a");
  const keys = Object.keys(obj);
  if (keys.length !== 2 || keys[0] !== input || keys[1] !== "\uFFFD") throw "keys";
  return obj[input] + obj["\uFFFD"] + tail;
}

// @pending ["wait:next"]
// @restore
// @fulfill ["wait:next", true]
// @fulfilled "1:1|2:2|done"

import { expect, wait } from 'test';

export async function run() {
  const source = Object.getOwnPropertyDescriptor(RegExp.prototype, 'source');
  const matchAll = RegExp.prototype[Symbol.matchAll];
  const iterator = "a1b2".matchAll(/(\d)/g);
  let trace = "";
  const first = iterator.next();
  trace += `${first.value[0]}:${first.value[1]}|`;
  await wait("next");
  const keys = Reflect.ownKeys(RegExp.prototype);
  expect(keys.slice(0, 14).join(',')).toBe('constructor,exec,test,toString,source,flags,global,ignoreCase,multiline,dotAll,unicode,hasIndices,unicodeSets,sticky');
  expect(keys[14]).toBe(Symbol.match);
  expect(keys[15]).toBe(Symbol.matchAll);
  expect(source.enumerable).toBe(false);
  expect(source.configurable).toBe(true);
  expect(source.set).toBe(undefined);
  expect(source.get).toBe(Object.getOwnPropertyDescriptor(RegExp.prototype, 'source').get);
  expect(Reflect.apply(source.get, /x/, [])).toBe('x');
  expect(matchAll).toBe(RegExp.prototype[Symbol.matchAll]);
  const second = iterator.next();
  trace += `${second.value[0]}:${second.value[1]}|`;
  if (iterator.next().done) trace += "done";
  return trace;
}

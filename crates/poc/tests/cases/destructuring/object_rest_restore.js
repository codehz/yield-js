// @pending ["wait:fulfill"]
// @restore
// @fulfill ["wait:fulfill", 7]
// @pending ["wait:reject"]
// @restore
// @reject ["wait:reject", "boom"]
// @fulfilled "CGR7:false:8|CGboom"

import { wait } from 'test';

export async function run() {
  let trace = "";
  const omit = Symbol("omit");
  const key = {
    [Symbol.toPrimitive]() { trace += "C"; return omit; },
  };
  const source = {
    get [omit]() { trace += "G"; return undefined; },
    get keep() { trace += "R"; return 8; },
  };
  let value;
  let rest;
  ({ [key]: value = await wait("fulfill"), ...rest } = source);
  trace += value + ":" + (omit in rest) + ":" + rest.keep + "|";
  try {
    ({ [key]: value = await wait("reject"), ...rest } = source);
    trace += "unexpected";
  } catch (error) {
    trace += error;
  }
  return trace;
}

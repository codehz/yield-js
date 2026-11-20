// @returns undefined

import { expect } from 'test';

function Target() {}
async function asyncTarget() {}

export function run() {
  const arrow = () => 0;
  const method = {method() {}}.method;
  const invalid = [arrow, method, asyncTarget, arrow.bind(null), method.bind(null), asyncTarget.bind(null), Symbol, {}];
  let touched = false;
  const list = {get length() { touched = true; return 0; }};
  for (const value of invalid) {
    let targetError;
    try { Reflect.construct(value, list); } catch (e) { targetError = e; }
    expect(targetError instanceof TypeError).toBe(true);
    let newTargetError;
    try { Reflect.construct(Target, list, value); } catch (e) { newTargetError = e; }
    expect(newTargetError instanceof TypeError).toBe(true);
    let newError;
    try { new value(); } catch (e) { newError = e; }
    expect(newError instanceof TypeError).toBe(true);
  }
  let undefinedNewTarget;
  try { Reflect.construct(Target, list, undefined); } catch (e) { undefinedNewTarget = e; }
  expect(undefinedNewTarget instanceof TypeError).toBe(true);
  let invalidList;
  try { Reflect.construct(Target, null); } catch (e) { invalidList = e; }
  expect(invalidList instanceof TypeError).toBe(true);
  const thrown = {};
  function Throws() { throw thrown; }
  let constructorError;
  try { Reflect.construct(Throws, []); } catch (e) { constructorError = e; }
  expect(constructorError).toBe(thrown);
  expect(touched).toBe(false);
}

// @returns undefined

import { expect } from 'test';

const moduleValue = 2;

function writeModuleValue() {
  moduleValue = 3;
}

export function run() {
  const value = 4;
  const writeOuter = () => { value = 5; };
  {
    let value = 6;
    const writeInner = () => { value = 7; };
    writeInner();
    expect(value).toBe(7);
  }
  let outerError;
  try {
    writeOuter();
  } catch (error) {
    outerError = error;
  }
  expect(outerError instanceof TypeError).toBe(true);

  let moduleError;
  try {
    writeModuleValue();
  } catch (error) {
    moduleError = error;
  }
  expect(moduleError instanceof TypeError).toBe(true);
  expect(value).toBe(4);
  expect(moduleValue).toBe(2);
}

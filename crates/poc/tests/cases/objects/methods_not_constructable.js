// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let error;
  try {
    await constructMethod();
  } catch (caught) {
    error = caught;
  }
  expect(typeof error).toBe("object");
  expect(error !== null).toBe(true);
  expect(error.name).toBe("TypeError");
  expect(error.message).toBe("value is not a constructor");
}

async function constructMethod() {
  const o = {
    m() {},
    C: function () { this.ok = true; }
  };
  if (!(new o.C()).ok) throw "function-valued property is not constructable";
  return new o.m();
}

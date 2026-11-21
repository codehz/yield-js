// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const trace = [];
  const key = { [Symbol.toPrimitive]() { trace.push("key"); return "0"; } };
  for (const target of ["str", 1, false, Symbol("target"), null, undefined]) {
    let error;
    try { Reflect.getOwnPropertyDescriptor(target, key); } catch (caught) { error = caught; }
    expect(error.name).toBe("TypeError");
  }
  let error;
  try { Object.getOwnPropertyDescriptor(null, key); } catch (caught) { error = caught; }
  expect(error.name).toBe("TypeError");
  expect(trace.length).toBe(0);
  expect(Object.getOwnPropertyDescriptor("str", key).value).toBe("s");
  expect(Reflect.getOwnPropertyDescriptor(Object("str"), key).value).toBe("s");
  expect(trace.join(",")).toBe("key,key");
}

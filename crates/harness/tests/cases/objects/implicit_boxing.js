// @fulfilled undefined

import { expect, gc } from 'test';

export async function run() {
  const source = "ab";
  let setterReceiver;
  Object.defineProperty(Object.prototype, "boxedAssignValue", {
    configurable: true,
    set(value) { gc(); setterReceiver = this; this._boxedAssignValue = value; },
  });
  const result = Object.assign(12, source, { boxedAssignValue: 7 });
  expect(Number.prototype.valueOf.call(result)).toBe(12);
  expect(result[0]).toBe("a");
  expect(result[1]).toBe("b");
  expect(setterReceiver).toBe(result);
  expect(result._boxedAssignValue).toBe(7);
  expect(Object.assign({}, "😀")[1]).toBe("\ude00");
  expect(Object.assign("xy").length).toBe(2);
  let assignError;
  try { Object.assign("xy", {0: "z"}); } catch (error) { assignError = error; }
  expect(assignError.name).toBe("TypeError");
  delete Object.prototype.boxedAssignValue;

  const descriptorTarget = "xy";
  const descriptor = Object.getOwnPropertyDescriptor(descriptorTarget, "0");
  expect(descriptor.value).toBe("x");
  expect(descriptor.enumerable).toBe(true);
  expect(descriptor.writable).toBe(false);
  expect(descriptor.configurable).toBe(false);
  expect(Object.hasOwn(descriptorTarget, "1")).toBe(true);
  expect(descriptorTarget.propertyIsEnumerable("1")).toBe(true);
  expect(descriptorTarget.propertyIsEnumerable("length")).toBe(false);
  expect(Object.hasOwn(descriptorTarget, "length")).toBe(true);
  expect(Object.getOwnPropertyDescriptor(descriptorTarget, "01")).toBe(undefined);
  const descriptors = Object.getOwnPropertyDescriptors("😀");
  expect(descriptors[0].value).toBe("\ud83d");
  expect(descriptors[1].value).toBe("\ude00");
  expect(descriptors.length.value).toBe(2);
  expect(descriptors.length.enumerable).toBe(false);
  expect(Object.hasOwn(Object.getOwnPropertyDescriptors(12), "length")).toBe(false);

  const inherited = Object.create({ inherited: 1 });
  inherited.own = 2;
  expect(Object.prototype.hasOwnProperty.call(inherited, "own")).toBe(true);
  expect(Object.prototype.hasOwnProperty.call(inherited, "inherited")).toBe(false);
  expect(Object.prototype.propertyIsEnumerable.call(inherited, "own")).toBe(true);
  expect(Object.prototype.propertyIsEnumerable.call(inherited, "inherited")).toBe(false);

  const boxed = Object.prototype.valueOf.call(12);
  expect(Number.prototype.valueOf.call(boxed)).toBe(12);
  expect(Object.prototype.valueOf.call(boxed)).toBe(boxed);
  expect(Object.prototype.valueOf.call(12) === boxed).toBe(false);
  expect(Object.prototype.toString.call(true)).toBe("[object Boolean]");
  expect(Object.prototype.toString.call(null)).toBe("[object Null]");
  const sentinel = {};
  const throwingKey = { [Symbol.toPrimitive]() { throw sentinel; } };
  for (const method of [Object.prototype.hasOwnProperty, Object.prototype.propertyIsEnumerable]) {
    let caught;
    try { Reflect.apply(method, null, [throwingKey]); } catch (error) { caught = error; }
    expect(caught).toBe(sentinel);
  }
}

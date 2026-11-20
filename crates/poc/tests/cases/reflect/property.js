// @pending ["wait:reflect"]
// @restore
// @fulfill ["wait:reflect", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

function Base() {}
Base.prototype = { inherited: 1, get seen() { return this.receiver; } };
Object.defineProperty(Base.prototype, "locked", { value: 1 });

export async function run() {
  const base = new Base();
  const receiver = { receiver: "child" };
  let trace = "";
  const target = Object.create(base, {
    value: { value: 2, writable: true, enumerable: true, configurable: true },
    get: { get() { return this.receiver; }, configurable: true },
    set: { set(value) { this.written = value; }, configurable: true },
    key: { get() { trace += "get;"; throw "getter"; }, configurable: true },
    primitive: { set(value) { if (this === undefined) trace += value; } },
    failure: { set(value) { throw value; } },
  });
  const array = [1];
  Object.defineProperty(array, "1", { value: 2, writable: false, configurable: false });
  const get = Reflect.get;
  const has = Reflect.has;
  const set = Reflect.set;
  const descriptor = Reflect.getOwnPropertyDescriptor;
  await wait("reflect");

  expect(get(target, "inherited")).toBe(1);
  expect(get(target, "get")).toBe(undefined);
  expect(get(target, "seen", receiver)).toBe("child");
  expect(get(target, "value", undefined)).toBe(2);
  expect(has(target, "inherited")).toBe(true);
  expect(has(target, "missing")).toBe(false);
  expect(has(target, "key")).toBe(true);
  expect(typeof descriptor(target, "key").get).toBe("function");
  expect(trace).toBe("");
  expect(set(target, "value", 3, undefined)).toBe(false);
  expect(target.value).toBe(2);
  expect(set(target, "value", 3, receiver)).toBe(true);
  expect(receiver.value).toBe(3);
  expect(target.value).toBe(2);
  expect(set(target, "set", 4, receiver)).toBe(true);
  expect(receiver.written).toBe(4);
  expect("written" in target).toBe(false);
  expect(set(target, "locked", 5, receiver)).toBe(false);
  expect(receiver.locked).toBe(undefined);
  expect(set(target, "value", 0, Object.preventExtensions({}))).toBe(false);
  expect(set(target, "value", 0, { set value(v) { throw "wrong setter"; } })).toBe(false);
  expect(set(target, "primitive", "primitive;", undefined)).toBe(true);
  expect(trace).toBe("primitive;");

  const own = descriptor(target, "value");
  expect(own.value).toBe(2);
  expect(!!own.writable).toBe(true);
  expect(!!own.enumerable).toBe(true);
  expect(!!own.configurable).toBe(true);
  expect(descriptor(target, "inherited")).toBe(undefined);
  expect(descriptor(array, "length").value).toBe(2);
  expect(set(array, "length", 1)).toBe(false);
  expect(array.length).toBe(2);
  expect(array[0]).toBe(1);
  expect(set(array, "1", 9)).toBe(false);
  expect(array[1]).toBe(2);
  expect(set(array, "2", 9)).toBe(true);
  expect(array[2]).toBe(9);

  const symbol = Symbol("reflect");
  const probe = { [Symbol.toPrimitive]() { trace += "key;"; return symbol; } };
  expect(!!set(target, probe, 9)).toBe(true);
  expect(!!has(target, symbol)).toBe(true);
  expect(get(target, symbol)).toBe(9);
  expect(descriptor(target, symbol).value).toBe(9);
  let invalidTarget;
  try { get(null, probe); } catch (error) { invalidTarget = error; }
  const invalidTargetName = invalidTarget && invalidTarget.name;
  expect(invalidTargetName).toBe("TypeError");
  trace += invalidTargetName + ";";
  let getterError;
  try { get(target, "key"); } catch (error) { getterError = error; }
  expect(getterError).toBe("getter");
  trace += getterError + ";";
  let setterError;
  try { set(target, "failure", "setter"); } catch (error) { setterError = error; }
  expect(setterError).toBe("setter");
  trace += setterError + ";";
  let lengthError;
  try { set(array, "length", -1); } catch (error) { lengthError = error; }
  const lengthErrorName = lengthError && lengthError.name;
  expect(lengthErrorName).toBe("RangeError");
  trace += lengthErrorName;
  expect(trace).toBe("primitive;key;TypeError;get;getter;setter;RangeError");
  expect(array.length).toBe(3);
}

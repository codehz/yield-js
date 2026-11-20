// @returns undefined

import { expect } from 'test';

export function run() {
  const empty = Object(null);
  expect(Object.getPrototypeOf(empty)).toBe(Object.prototype);
  expect(Object.getPrototypeOf(Object(undefined))).toBe(Object.prototype);
  expect(Object.getPrototypeOf(new Object())).toBe(Object.prototype);
  expect(Object(null) === empty).toBe(false);

  const symbol = Symbol("token");
  const primitives = ["abc", 1, false, symbol];
  const prototypes = [String.prototype, Number.prototype, Boolean.prototype, Symbol.prototype];
  const names = ["String", "Number", "Boolean", "Symbol"];
  const texts = ["abc", "1", "false", "Symbol(token)"];
  for (let i = 0; i < primitives.length; i++) {
    const value = primitives[i];
    const box = Object(value);
    const constructed = new Object(value);
    expect(typeof box).toBe("object");
    expect(box === value).toBe(false);
    expect(box === Object(value)).toBe(false);
    expect(box === constructed).toBe(false);
    expect(Object(box)).toBe(box);
    expect(constructed.valueOf()).toBe(value);
    expect(box.valueOf()).toBe(value);
    expect(box.toString()).toBe(texts[i]);
    expect(Object.prototype.toString.call(box)).toBe("[object " + names[i] + "]");
    expect(Object.getPrototypeOf(box)).toBe(prototypes[i]);
    expect(Object.getPrototypeOf(value)).toBe(prototypes[i]);
    expect(Object.setPrototypeOf(value, null)).toBe(value);
    expect(box.hasOwnProperty("valueOf")).toBe(false);
    expect(box.hasOwnProperty("toString")).toBe(false);
  }

  const string = new String("A😀");
  expect(string.valueOf()).toBe("A😀");
  expect(string.toString()).toBe("A😀");
  expect(string.length).toBe(3);
  expect(string[0]).toBe("A");
  expect("2" in string).toBe(true);
  expect(Reflect.has(string, "length")).toBe(true);
  expect(Reflect.has(string, "3")).toBe(false);

  let indexError;
  try {
    string[0] = "B";
  } catch (caught) {
    indexError = caught;
  }
  expect(indexError instanceof TypeError).toBe(true);
  expect(string[0]).toBe("A");
  let lengthError;
  try {
    string.length = 0;
  } catch (caught) {
    lengthError = caught;
  }
  expect(lengthError instanceof TypeError).toBe(true);
  expect(string.length).toBe(3);
  string.extra = 42;
  expect(string.extra).toBe(42);
  expect("extra" in string).toBe(true);

  const number = new Number(1);
  const boolean = new Boolean(false);
  expect(number === 1).toBe(false);
  expect(number.valueOf()).toBe(1);
  expect(number.toString()).toBe("1");
  expect(number.toFixed(1)).toBe("1.0");
  expect(typeof boolean).toBe("object");
  expect(boolean.valueOf()).toBe(false);
  expect(boolean.toString()).toBe("false");
  expect(Boolean()).toBe(false);
  expect(Boolean(0)).toBe(false);
  expect(Boolean("false")).toBe(true);
  expect(Boolean(boolean)).toBe(true);
  expect(Object(symbol).description).toBe("token");
  expect((true).valueOf()).toBe(true);
  const {valueOf} = true;
  expect(valueOf).toBe((true).valueOf);
  expect(symbol.description).toBe('token');
  const {description} = symbol;
  expect(description).toBe('token');

  let receiver;
  let argument;
  let keyCount = 0;
  Object.defineProperty(Boolean.prototype, 'x', {
    set(value) { receiver = this; argument = value; }
  });
  (true).x = 7;
  expect(receiver).toBe(true);
  expect(argument).toBe(7);
  (false)[{toString() { keyCount++; return 'x'; }}] = 8;
  expect(keyCount).toBe(1);
  expect(receiver).toBe(false);
  expect(argument).toBe(8);

  const sentinel = {};
  Object.defineProperty(Boolean.prototype, 'throwing', {set(value) { throw sentinel; }});
  Object.defineProperty(Boolean.prototype, 'getterOnly', {get() { return this; }});
  let setterCalls = 0;
  Object.defineProperty(String.prototype, '0', {set(value) { setterCalls++; }});
  Object.defineProperty(String.prototype, '4', {get() { return this; }});
  expect('abc'[4]).toBe('abc');
  const {4: inherited} = 'abc';
  expect(inherited).toBe('abc');
  for (const action of [
    () => { (true).getterOnly = 1; },
    () => { (true).missing = 1; },
    () => { 'abc'[0] = 'x'; },
    () => 'length' in 'abc',
  ]) {
    let error;
    try { action(); } catch (caught) { error = caught; }
    expect(error instanceof TypeError).toBe(true);
  }
  expect(setterCalls).toBe(0);
  let error;
  try { (true).throwing = 1; } catch (caught) { error = caught; }
  expect(error).toBe(sentinel);
  expect(String(123)).toBe("123");
  expect(Number("2")).toBe(2);

  let symbolError;
  try {
    new Symbol("not a constructor");
  } catch (caught) {
    symbolError = caught;
  }
  expect(symbolError instanceof TypeError).toBe(true);
}

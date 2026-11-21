// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const key = Symbol.hasInstance;
  expect(key.description).toBe('Symbol.hasInstance');
  const symbolDescriptor = Object.getOwnPropertyDescriptor(Symbol, 'hasInstance');
  expect(symbolDescriptor.value).toBe(key);
  expect(symbolDescriptor.writable).toBe(false);
  expect(symbolDescriptor.enumerable).toBe(false);
  expect(symbolDescriptor.configurable).toBe(false);

  let lookups = 0;
  let calls = 0;
  const argument = {};
  const receiver = Object.create({
    get [key]() {
      lookups++;
      expect(this).toBe(receiver);
      return function(value, extra) {
        calls++;
        expect(this).toBe(receiver);
        expect(value).toBe(argument);
        expect(extra).toBe(undefined);
        return {};
      };
    }
  });
  expect(argument instanceof receiver).toBe(true);
  expect(lookups).toBe(1);
  expect(calls).toBe(1);

  const sentinel = {};
  let error;
  try { argument instanceof { get [key]() { throw sentinel; } }; }
  catch (caught) { error = caught; }
  expect(error).toBe(sentinel);
  error = undefined;
  try { argument instanceof { [key]() { throw sentinel; } }; }
  catch (caught) { error = caught; }
  expect(error).toBe(sentinel);
  for (const rhs of [{ [key]: 1 }, { [key]: null }, 1]) {
    error = undefined;
    try { argument instanceof rhs; } catch (caught) { error = caught; }
    expect(error instanceof TypeError).toBe(true);
  }
  expect(argument instanceof { [key]() { return 0; } }).toBe(false);

  let traps = 0;
  const proxy = new Proxy({}, {
    get(target, property, receiver) {
      traps++;
      expect(property).toBe(key);
      expect(receiver).toBe(proxy);
      return function(value) { return this === proxy && value === 7; };
    }
  });
  expect(7 instanceof proxy).toBe(true);
  expect(traps).toBe(1);

  function Customized() {}
  const bound = Customized.bind(null).bind(null);
  Object.defineProperty(Customized, key, {
    value: function(value) { return this === Customized && value === 7; }
  });
  expect(7 instanceof bound).toBe(true);
  expect(argument instanceof bound).toBe(false);

  function Ordinary() {}
  const instance = new Ordinary();
  const functionPrototype = Object.getPrototypeOf(Ordinary);
  const descriptor = Object.getOwnPropertyDescriptor(functionPrototype, key);
  const ordinary = descriptor.value;
  expect(descriptor.writable).toBe(false);
  expect(descriptor.enumerable).toBe(false);
  expect(descriptor.configurable).toBe(false);
  expect(ordinary.call(receiver, argument)).toBe(false);
  expect(ordinary.call(Customized, 7)).toBe(false);
  expect(ordinary.call(bound, 7)).toBe(true);
  expect(instance instanceof Ordinary.bind(null)).toBe(true);
  Object.defineProperty(Ordinary, key, {value: null});
  expect(instance instanceof Ordinary).toBe(true);

  await wait('a');
  expect(Symbol.hasInstance).toBe(key);
  expect(argument instanceof receiver).toBe(true);
  expect(lookups).toBe(2);
  expect(calls).toBe(2);
  expect(7 instanceof bound).toBe(true);
  expect(ordinary.call(Ordinary, instance)).toBe(true);
}

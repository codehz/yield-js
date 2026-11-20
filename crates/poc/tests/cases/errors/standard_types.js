// @returns undefined

import { expect } from 'test';

export function run() {
  const types = [
    [RangeError, "RangeError"], [ReferenceError, "ReferenceError"],
    [SyntaxError, "SyntaxError"], [EvalError, "EvalError"],
    [URIError, "URIError"],
  ];
  for (const [Ctor, name] of types) {
    const called = Ctor("custom");
    const made = new Ctor();
    expect(called instanceof Ctor).toBe(true);
    expect(made instanceof Ctor).toBe(true);
    expect(called.name).toBe(name);
    expect(made.name).toBe(name);
    expect(called.message).toBe("custom");
    expect(made.message).toBe("");
    expect(Object.getPrototypeOf(Ctor.prototype)).toBe(Error.prototype);
    expect(Ctor.prototype.constructor).toBe(Ctor);
  }
  const list = new AggregateError([1, 2], "array");
  const string = AggregateError("ab", "string");
  const iterable = AggregateError({
    [Symbol.iterator]() {
      let i = 0;
      return { next() { return i < 2 ? {value: ++i, done: false} : {done: true}; } };
    }
  });
  expect(list instanceof AggregateError).toBe(true);
  expect(list.name).toBe("AggregateError");
  expect(list.message).toBe("array");
  expect(list.errors.length).toBe(2);
  expect(list.errors[0]).toBe(1);
  expect(list.errors[1]).toBe(2);
  expect(string.errors.length).toBe(2);
  expect(string.errors[0]).toBe("a");
  expect(string.errors[1]).toBe("b");
  expect(iterable.errors.length).toBe(2);
  expect(iterable.errors[0]).toBe(1);
  expect(iterable.errors[1]).toBe(2);
  expect(AggregateError().errors.length).toBe(0);
  expect(AggregateError(undefined).errors.length).toBe(0);
  let aggregateError;
  try { AggregateError(null); } catch (error) { aggregateError = error; }
  expect(aggregateError instanceof TypeError).toBe(true);
}

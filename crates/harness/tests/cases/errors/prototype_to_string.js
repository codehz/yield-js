// @returns undefined

import { expect } from 'test';

export function run() {
  expect(new Error("boom").toString()).toBe("Error: boom");
  expect(new Error().toString()).toBe("Error");
  expect(new TypeError("bad").toString()).toBe("TypeError: bad");
  expect(new RangeError().toString()).toBe("RangeError");
  expect(new AggregateError([], "multi").toString()).toBe("AggregateError: multi");

  const named = new Error("custom");
  named.name = "Custom";
  expect(named.toString()).toBe("Custom: custom");

  const noMessage = new Error();
  noMessage.message = undefined;
  expect(noMessage.toString()).toBe("Error");

  const messageAsNumber = new Error(3);
  expect(messageAsNumber.toString()).toBe("Error: 3");

  // Missing own message falls through to the inherited property.
  const inherited = Object.create(new Error("from proto"));
  expect(inherited.toString()).toBe("Error: from proto");

  // Dynamic property lookup.
  const dynamic = new Error();
  dynamic.name = 1;
  dynamic.message = 2;
  expect(dynamic.toString()).toBe("1: 2");
  delete dynamic.name;
  dynamic.message = 4;
  expect(dynamic.toString()).toBe("Error: 4");

  // A throwing name getter aborts before the message is read.
  const throwingName = new Error();
  let messageRead = false;
  Object.defineProperty(throwingName, "name", {
    get() { throw new TypeError("name boom"); },
  });
  Object.defineProperty(throwingName, "message", {
    get() { messageRead = true; return "later"; },
  });
  let caught;
  try { throwingName.toString(); } catch (error) { caught = error; }
  expect(caught instanceof TypeError).toBe(true);
  expect(caught.message).toBe("name boom");
  expect(messageRead).toBe(false);

  // Non-callable message conversion also aborts.
  const throwingMessage = new Error();
  Object.defineProperty(throwingMessage, "message", {
    get() { throw new TypeError("message boom"); },
  });
  try { throwingMessage.toString(); expect(true).toBe(false); } catch (error) {
    expect(error instanceof TypeError).toBe(true);
    expect(error.message).toBe("message boom");
  }

  // toString is writable, non-enumerable, configurable.
  expect(Object.getOwnPropertyDescriptor(Error.prototype, "toString")).toEqual({
    value: Error.prototype.toString,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // Subclass prototypes inherit the same function object.
  expect(TypeError.prototype.toString).toBe(Error.prototype.toString);
}

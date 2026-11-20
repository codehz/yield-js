// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const key = Symbol("key");
  let hints = "";
  const value = {
    [Symbol.toPrimitive](hint) {
      hints += hint + ";";
      if (hint === "string") return key;
      return hint === "number" ? 7 : 9;
    },
    valueOf() { throw "ordinary fallback"; }
  };
  const target = { [value]: 3 };
  expect(target[key]).toBe(3);
  expect(+value).toBe(7);
  expect(value + 1).toBe(10);
  expect(value == 9).toBe(true);
  let thrown;
  try {
    `${value}`;
  } catch (error) {
    thrown = error;
  }
  expect(thrown?.name).toBe("TypeError");
  expect(hints).toBe("string;number;default;default;string;");
  const fallback = { [Symbol.toPrimitive]: null, toString() { return "fallback"; } };
  expect(`${fallback}`).toBe("fallback");
  for (const bad of [1, function () { return {}; }]) {
    let thrown;
    try {
      +{ [Symbol.toPrimitive]: bad, valueOf() { return 1; } };
    } catch (error) {
      thrown = error;
    }
    expect(thrown?.name).toBe("TypeError");
  }
}

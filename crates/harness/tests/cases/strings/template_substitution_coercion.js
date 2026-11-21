// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let order = "";
  function substitution(label, value) {
    order += "eval:" + label + ";";
    return {
      toString() { order += "string:" + label + ";"; return value; },
      valueOf() { order += "number:" + label + ";"; return 0; }
    };
  }

  expect(`<${substitution("a", "A")}${substitution("b", "B")}>`).toBe("<AB>");
  expect(order).toBe("eval:a;string:a;eval:b;string:b;");

  order = "";
  const symbol = Symbol("template");
  let symbolErrorName;
  try {
    `${symbol}${substitution("late", "L")}`;
  } catch (error) {
    symbolErrorName = error.name;
  }
  expect(symbolErrorName).toBe("TypeError");
  expect(order).toBe("");

  let substitutionErrorName;
  try {
    `${substitution("symbol", symbol)}${substitution("late", "L")}`;
  } catch (error) {
    substitutionErrorName = error.name;
  }
  expect(substitutionErrorName).toBe("TypeError");
  expect(order).toBe("eval:symbol;string:symbol;");
}

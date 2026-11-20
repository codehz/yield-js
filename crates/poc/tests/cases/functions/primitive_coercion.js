// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  function callable() {}
  const text = `${callable}`;
  expect(typeof text).toBe("string");
  expect(callable + "").toBe(text);
  expect(`${callable}`).toBe(text);
  const numeric = +callable;
  expect(numeric !== numeric).toBe(true);

  let calls = "";
  callable.valueOf = function () { calls += "v"; return this === callable ? 6 : 0; };
  callable.toString = function () { calls += "s"; return this === callable ? "fn" : "wrong"; };
  expect(+callable).toBe(6);
  expect(callable + 1).toBe(7);
  expect(`${callable}`).toBe("fn");

  callable.valueOf = function () { calls += "o"; return this; };
  expect(callable + "!").toBe("fn!");
  expect(calls).toBe("vvsos");
  callable.toString = function () { calls += "t"; return this; };
  try {
    `${callable}`;
  } catch (error) {
    expect(error.name).toBe("TypeError");
    expect(calls).toBe("vvsosto");
    return;
  }
  expect("coercion succeeded").toBe("coercion threw");
}

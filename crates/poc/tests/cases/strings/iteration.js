// @fulfilled undefined

import { expect } from 'test';

function collect(a, b, c, d, e, f) {
  return [a, b, c, d, e, f].join("|");
}

export async function run() {
  const text = "Aé汉😀e\u0301";
  const expected = "A|é|汉|😀|e|\u0301";
  let loop = [];
  for (const ch of text) {
    loop = [...loop, ch];
  }
  const symbol = Symbol.iterator;
  const iterator = text[symbol]();
  const other = text[symbol]();
  const empty = ""[symbol]().next();
  const next = iterator.next;
  let errorName;
  try {
    next();
  } catch (error) {
    errorName = error.name;
  }
  const first = iterator.next();
  const tail = [...iterator].join("|");
  const done = iterator.next();
  const again = iterator.next();
  expect(loop.join("|")).toBe(expected);
  expect([...text].join("|")).toBe(expected);
  expect(collect(...text)).toBe(expected);
  expect(text[symbol]).toBe(""[symbol]);
  expect(iterator[symbol]()).toBe(iterator);
  expect(other.next().value).toBe("A");
  expect(first.value).toBe("A");
  expect(!first.done).toBe(true);
  expect(tail).toBe("é|汉|😀|e|\u0301");
  expect(!!empty.done).toBe(true);
  expect(empty.value).toBe(undefined);
  expect(!!done.done).toBe(true);
  expect(done.value).toBe(undefined);
  expect(!!again.done).toBe(true);
  expect(again.value).toBe(undefined);
  expect(errorName).toBe("TypeError");
}

// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let caught;
  try {
    await Promise.any([Promise.reject("first"), Promise.reject("second"), Promise.reject("third")]);
  } catch (error) {
    caught = error;
  }
  expect(caught instanceof AggregateError).toBe(true);
  expect(caught.errors.length).toBe(3);
  expect(caught.errors[0]).toBe("first");
  expect(caught.errors[1]).toBe("second");
  expect(caught.errors[2]).toBe("third");
  expect(await Promise.any([Promise.reject("no"), Promise.resolve("winner")])).toBe("winner");
  let emptyError;
  try { await Promise.any([]); }
  catch (error) { emptyError = error; }
  expect(emptyError instanceof AggregateError).toBe(true);
  expect(emptyError.errors.length).toBe(0);
}

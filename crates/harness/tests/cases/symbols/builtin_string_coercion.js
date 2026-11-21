// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let hints = "";
  const arg = {
    [Symbol.toPrimitive](hint) {
      if (this !== arg) throw "wrong receiver";
      hints += hint + ";";
      return "converted";
    }
  };
  expect(Error(arg).message).toBe("converted");
  expect(new TypeError(arg).message).toBe("converted");
  const fresh = Symbol(arg);
  expect(Symbol.keyFor(fresh)).toBe(undefined);
  expect(Symbol.for(arg)).toBe(Symbol.for("converted"));
  expect(hints).toBe("string;string;string;string;");
  expect(Object.getOwnPropertyDescriptor(Error(undefined), "message")).toBe(undefined);
  expect(Symbol.keyFor(Symbol.for())).toBe("undefined");

  const marker = {};
  const throwing = { get toString() { throw marker; } };
  let error;
  try { new Error(throwing); } catch (e) { error = e; }
  expect(error).toBe(marker);
  error = undefined;
  try { Symbol(throwing); } catch (e) { error = e; }
  expect(error).toBe(marker);
  error = undefined;
  try { Symbol.for(throwing); } catch (e) { error = e; }
  expect(error).toBe(marker);
  const symbol = { toString() { return fresh; } };
  error = undefined;
  try { TypeError(symbol); } catch (e) { error = e; }
  expect(error?.name).toBe("TypeError");
  error = undefined;
  try { Symbol.for(symbol); } catch (e) { error = e; }
  expect(error?.name).toBe("TypeError");
}

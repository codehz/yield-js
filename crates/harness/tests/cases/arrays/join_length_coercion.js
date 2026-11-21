// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const method = [].join;
  function join(source, separator) {
    source.join = method;
    return source.join(separator);
  }
  let order = "";
  const source = {
    get length() {
      order += "l";
      return { valueOf() { order += "v"; return " 2.9 "; } };
    },
    get 0() { order += "0"; return "a"; },
    get 1() { order += "1"; return "b"; },
    get 2() { throw "length was not truncated"; }
  };
  const sep = { toString() { order += "s"; return "-"; } };
  expect(join(source, sep)).toBe("a-b");
  expect(order).toBe("lvs01");
  expect(join({ length: true, 0: "x" })).toBe("x");
  expect(join({ length: "\uFEFF0x2", 0: "x", 1: "y" })).toBe("x,y");
  expect(join({ length: "inf", get 0() { throw "invalid length"; } })).toBe("");

  const marker = {};
  let coercionError;
  try { join({ length: { valueOf() { throw marker; } } }, sep); } catch (e) { coercionError = e; }
  expect(coercionError).toBe(marker);
  expect(order).toBe("lvs01");
  let elementError;
  try { join({ length: "Infinity", get 0() { throw marker; } }); } catch (e) { elementError = e; }
  expect(elementError).toBe(marker);
  let symbolError;
  try { join({ length: Symbol() }); } catch (e) { symbolError = e; }
  expect(symbolError?.name).toBe("TypeError");
}

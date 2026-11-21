// @returns undefined

import { expect } from 'test';

export function run() {
  const re = /(?<=x)(?<n>a)\k<n>/d;
  const result = re.exec("xaa");
  const range = result.indices.groups.n;
  let coerced = false;
  let escapeError;
  try {
    RegExp.escape({ toString() { coerced = true; return "x"; } });
  } catch (error) {
    escapeError = error;
  }
  expect(escapeError instanceof TypeError).toBe(true);
  expect(coerced).toBe(false);
  expect(re.hasIndices).toBe(true);
  expect(re.unicodeSets).toBe(false);
  expect(re.flags).toBe("d");
  expect(range[0]).toBe(1);
  expect(range[1]).toBe(2);
  expect(result.indices[1]).toBe(range);
  expect(RegExp.escape("a-b")).toBe("\\x61\\x2db");
}

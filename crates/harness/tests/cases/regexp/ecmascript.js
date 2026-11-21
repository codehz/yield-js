// @returns undefined

import { expect } from 'test';

export function run() {
  const lookbehind = /(?<=x)(?<n>a)\k<n>/.exec("xaa");
  const unicode = /./u.exec("😀");
  const sets = /[\p{ASCII}&&\p{Letter}]/v;
  const scoped = /(?i:a)b/;
  expect(lookbehind !== null).toBe(true);
  expect(lookbehind.index).toBe(1);
  expect(lookbehind.groups.n).toBe("a");
  expect(unicode[0]).toBe("😀");
  expect(unicode[0].length).toBe(2);
  expect(sets.test("A")).toBe(true);
  expect(sets.test("1")).toBe(false);
  expect(scoped.test("Ab")).toBe(true);
  expect(scoped.test("AB")).toBe(false);
  const rejectedIncompatibleFlags = (() => {
    try {
      new RegExp("", "uv");
      return false;
    } catch (error) {
      return error instanceof SyntaxError;
    }
  })();
  expect(rejectedIncompatibleFlags).toBe(true);
}

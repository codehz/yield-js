// @returns true

export function run() {
  const lookbehind = /(?<=x)(?<n>a)\k<n>/.exec("xaa");
  const unicode = /./u.exec("😀");
  const sets = /[\p{ASCII}&&\p{Letter}]/v;
  const scoped = /(?i:a)b/;
  return lookbehind !== null && lookbehind.index === 1 && lookbehind.groups.n === "a" &&
    unicode[0] === "😀" && unicode[0].length === 2 &&
    sets.test("A") && !sets.test("1") && scoped.test("Ab") && !scoped.test("AB") &&
    (() => { try { new RegExp("", "uv"); return false; } catch (error) { return error instanceof SyntaxError; } })();
}

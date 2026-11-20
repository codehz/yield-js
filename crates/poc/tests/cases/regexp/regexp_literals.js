// @returns true

export function run() {
  function fresh() { return /ab+c/i; }
  const re = fresh();
  const captured = /(?<word>ab+c)(!)/i.exec("--ABBC!");
  const modes = /a/gimsy;
  return re.test("ABBC") && !re.test("AC") &&
    re !== fresh() && re instanceof RegExp && re.lastIndex === 0 &&
    re.source === "ab+c" && re.flags === "i" && re.toString() === "/ab+c/i" &&
    modes.flags === "gimsy" && modes.global && modes.ignoreCase &&
    modes.multiline && modes.dotAll && modes.sticky && !modes.unicode &&
    /^b/m.test("a\nb") && /a.b/s.test("a\nb") &&
    Array.isArray(captured) && captured.length === 3 &&
    captured[0] === "ABBC!" && captured[1] === "ABBC" && captured[2] === "!" &&
    captured.index === 2 && captured.input === "--ABBC!" &&
    captured.groups.word === "ABBC" &&
    Object.getOwnPropertyDescriptor(captured, "index").value === 2 &&
    Object.getOwnPropertyDescriptor(captured, "input").value === "--ABBC!" &&
    Object.getOwnPropertyDescriptor(captured, "groups").value === captured.groups &&
    /a/.exec("a").groups === undefined;
}

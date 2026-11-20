// @returns true

export function run() {
  const re = /(?<=x)(?<n>a)\k<n>/d;
  const result = re.exec("xaa");
  const range = result.indices.groups.n;
  let coerced = false;
  try { RegExp.escape({ toString() { coerced = true; return "x"; } }); return false; }
  catch (error) {
    return error instanceof TypeError && !coerced && re.hasIndices && !re.unicodeSets &&
      re.flags === "d" && range[0] === 1 && range[1] === 2 &&
      result.indices[1] === range && RegExp.escape("a-b") === "\\x61\\x2db";
  }
}

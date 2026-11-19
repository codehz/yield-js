// @returns true

function syntaxError(pattern, flags) {
  try {
    new RegExp(pattern, flags);
    return false;
  } catch (error) {
    return error.name === "SyntaxError";
  }
}

export function run() {
  const re = new RegExp({ toString() { return "ab+c"; } }, {
    toString() { return "ig"; },
  });
  const copy = new RegExp(re);
  const override = RegExp(re, "y");
  return re.flags === "gi" && re.test("ABBC") && re.lastIndex === 4 &&
    RegExp(re) === re && copy !== re && copy.source === re.source &&
    copy.flags === re.flags && copy.lastIndex === 0 &&
    override !== re && override.source === re.source && override.flags === "y" &&
    RegExp("abc").test("abc") && syntaxError("[", "") &&
    syntaxError("abc", "gg") && syntaxError("abc", "d") &&
    syntaxError("a(?=b)", "") && syntaxError("(a)\\1", "");
}

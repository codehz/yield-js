// @returns "ok"

function eq(actual, expected) {
  if (actual !== expected) throw new Error("Expected " + expected + ", got " + actual);
}

function invalid(fn, input) {
  try { fn(input); } catch (error) {
    eq(error instanceof DOMException, true);
    eq(error.name, "InvalidCharacterError");
    eq(error.code, DOMException.INVALID_CHARACTER_ERR);
    return;
  }
  throw new Error("Accepted invalid input");
}

export function run() {
  eq(globalThis.atob, atob);
  eq(globalThis.btoa, btoa);
  eq(btoa("\x00\x80\xFF"), "AID/");
  eq(btoa(undefined), "dW5kZWZpbmVk");
  eq(btoa("a"), "YQ==");
  eq(btoa("ab"), "YWI=");
  eq(atob(" A\tI\nD\f/\r"), "\x00\x80\xFF");
  eq(atob(" Y R = = "), "a");
  eq(atob("YR"), "a");
  eq(atob("YWI"), "ab");
  eq(atob(" \r\n"), "");
  for (const input of ["A", "YQ=", "YWI==", "=YQ=", "YQ===", "====", "YQ==A", "YQ\v", "YQ\u00A0", "_w", "\u0100"]) {
    invalid(atob, input);
  }
  for (const input of ["\u0100", "\uD800", "😀"]) invalid(btoa, input);
  for (const fn of [atob, btoa]) {
    let typeError = false;
    try { fn(); } catch (error) { typeError = error instanceof TypeError; }
    eq(typeError, true);
    let created = false;
    try { new fn(""); created = true; } catch (error) { eq(error instanceof TypeError, true); }
    eq(created, false);
    let thrown;
    try { fn({ toString() { throw "conversion"; } }); } catch (error) { thrown = error; }
    eq(thrown, "conversion");
  }
  return "ok";
}

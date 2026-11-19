// @returns true

export function run() {
  let log = "";
  const object = {x: 1};
  const key = {
    [Symbol.toPrimitive](hint) { log += hint; return "x"; }
  };
  function base() { log += "base/"; return object; }
  function index() { log += "key/"; return key; }
  const absent = null;
  if (!delete ((absent?.child[index()])) || log !== "") return false;
  if (!delete ((base()[index()])) || log !== "base/key/string" || "x" in object) return false;
  if (!delete object.missing?.(index()) || log !== "base/key/string") return false;
  try {
    delete (absent?.child)[index()];
    return false;
  } catch (e) {
    if (e.name !== "TypeError" || log !== "base/key/stringkey/") return false;
  }
  Object.defineProperty(object, "x", {value: 2});
  if (Reflect.deleteProperty(object, key) || log !== "base/key/stringkey/string" || object.x !== 2) return false;
  try {
    delete object.x;
    return false;
  } catch (e) {
    if (e.name !== "TypeError") return false;
  }
  const bad = {toString() { throw 42; }};
  try {
    delete object[bad];
    return false;
  } catch (e) {
    if (e !== 42) return false;
  }
  try {
    Reflect.deleteProperty(1, bad);
    return false;
  } catch (e) {
    if (e.name !== "TypeError") return false;
  }
  try {
    delete "😀"[1];
    return false;
  } catch (e) {
    if (e.name !== "TypeError") return false;
  }
  return delete "😀"[2] && delete "ab"["01"] && delete true.x;
}

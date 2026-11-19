// @fulfilled true

async function run() {
  const define = Object.defineProperties;
  const target = {};
  const error = {};
  try {
    define(target, { a: { value: 1 }, get b() { target.sideEffect = 2; throw error; } });
    return false;
  } catch (caught) {
    if (caught !== error || "a" in target || target.sideEffect !== 2) return false;
  }
  Object.defineProperty(target, "fixed", { value: 1 });
  let converted = false;
  try {
    define(target, {
      a: { value: 3 },
      fixed: { value: 2 },
      c: { get value() { converted = true; return 4; } },
    });
    return false;
  } catch (caught) {
    if (caught.name !== "TypeError" || !converted || target.a !== 3 || target.fixed !== 1 || "c" in target) return false;
  }
  try {
    define(target, { b: { value: 5 }, invalid: 0 });
    return false;
  } catch (caught) {
    if (caught.name !== "TypeError" || "b" in target) return false;
  }
  try {
    define(target, "x");
    return false;
  } catch (caught) {
    if (caught.name !== "TypeError" || "0" in target) return false;
  }
  try {
    define(target, null);
    return false;
  } catch (caught) {
    if (caught.name !== "TypeError") return false;
  }
  try {
    define(1, { get a() { throw error; } });
    return false;
  } catch (caught) {
    if (caught.name !== "TypeError") return false;
  }
  return define(target, 1) === target && define(target, "") === target;
}

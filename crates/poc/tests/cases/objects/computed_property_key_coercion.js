// @fulfilled true

async function run() {
  let order = "";
  const symbol = Symbol("key");
  const key = {
    toString() { order += "key;"; return symbol; },
    valueOf() { order += "wrong;"; return "wrong"; }
  };
  function value() { order += "value;"; return 4; }

  const target = { [key]: value() };
  if (order !== "key;value;" || target[symbol] !== 4) return false;
  order = "";
  if (target[key] !== 4 || !(key in target) || order !== "key;key;") return false;

  order = "";
  const sum = (target[key] += value());
  if (sum !== 8 || target[symbol] !== 8 || order !== "key;value;") return false;

  order = "";
  Object.defineProperty(target, key, { value: 10 });
  if (!target.hasOwnProperty(key) || Object.getOwnPropertyDescriptor(target, key).value !== 10
      || target[symbol] !== 10 || order !== "key;key;key;") return false;

  const fallback = {
    toString() { return {}; },
    valueOf() { return 9; }
  };
  target[fallback] = 3;
  if (target["9"] !== 3 || !(fallback in target)) return false;

  order = "";
  function invalidRhs() { order += "rhs;"; return 1; }
  try {
    key in invalidRhs();
    return false;
  } catch (error) {
    if (error.name !== "TypeError" || order !== "rhs;") return false;
  }

  order = "";
  const failure = {};
  const throwing = { toString() { throw failure; } };
  try {
    ({ [throwing]: value() });
    return false;
  } catch (error) {
    return error === failure && order === "";
  }
}

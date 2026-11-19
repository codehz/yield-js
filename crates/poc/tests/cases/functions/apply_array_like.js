// @fulfilled true

export async function run() {
  // Plain array-like object.
  if (first.apply(null, { 0: "a", length: 1 }) !== "a") return false;

  // A real Array still works.
  if (first.apply(null, ["x", "y"]) !== "x") return false;

  // null and undefined mean an empty argument list.
  if (first.apply(null) !== undefined || first.apply(null, null) !== undefined ||
    first.apply(null, undefined) !== undefined) return false;

  // An arguments object works.
  function spreadArgs() {
    return count.apply(null, arguments);
  }
  if (spreadArgs("p", "q", "r") !== 3) return false;

  // Inherited index and length properties are used.
  const proto = { 0: "inherited", length: 2 };
  const child = Object.create(proto);
  child[1] = "own";
  if (pair.apply(null, child) !== "inherited-own") return false;

  // A getter access from the array-like propagates its throw.
  const throwingLength = {
    get length() {
      throw new RangeError("length boom");
    },
  };
  try {
    first.apply(null, throwingLength);
    return false;
  } catch (e) {
    if (e.name !== "RangeError" || e.message !== "length boom") return false;
  }
  const throwingIndex = { length: 2, 0: "ok" };
  Object.defineProperty(throwingIndex, "1", {
    get() {
      throw new TypeError("index boom");
    },
  });
  try {
    first.apply(null, throwingIndex);
    return false;
  } catch (e) {
    if (e.name !== "TypeError" || e.message !== "index boom") return false;
  }

  // A primitive argument list throws a catchable TypeError.
  try {
    first.apply(null, "ab");
    return false;
  } catch (e) {
    if (e.name !== "TypeError") return false;
  }
  try {
    first.apply(null, 5);
    return false;
  } catch (e) {
    if (e.name !== "TypeError") return false;
  }

  // ToLength coercion of the length value.
  const weird = { length: "2.9", 0: "u", 1: "v" };
  return pair.apply(null, weird) === "u-v";
}

function first(a) {
  return a;
}

function pair(a, b) {
  return a + "-" + b;
}

function count(...args) {
  return args.length;
}

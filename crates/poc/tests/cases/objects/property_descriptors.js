// @pending ["wait:descriptor"]
// @restore
// @fulfill ["wait:descriptor", 0]
// @fulfilled true

function Box() {}

async function run() {
  const symbol = Symbol("data");
  const object = {
    [symbol]: 1,
    get x() { throw "descriptor lookup read the getter"; },
    set x(value) { this[symbol] = value; },
  };
  const describe = Object.getOwnPropertyDescriptor;
  const before = describe(object, "x");
  const array = [1];
  await wait("descriptor");
  const accessor = describe(object, "x");
  if (accessor.get !== before.get || accessor.set !== before.set ||
      typeof accessor.get !== "function" || !accessor.enumerable || !accessor.configurable ||
      "writable" in accessor || "value" in accessor) return false;
  object.x = 7;
  const data = describe(object, symbol);
  if (data.value !== 7 || !data.writable || !data.enumerable || !data.configurable ||
      "get" in data || describe(object, "hasOwnProperty") !== undefined) return false;
  data.value = 100;
  if (object[symbol] !== 7) return false;
  const length = describe(array, "length");
  const element = describe(array, "0");
  if (length.value !== 1 || !length.writable || length.enumerable || length.configurable ||
      !element.enumerable || !element.configurable || !element.writable) return false;
  const method = describe(Object, "keys");
  if (method.value !== Object.keys || !method.writable || method.enumerable ||
      !method.configurable) return false;
  const fixed = describe(Object, "prototype");
  if (fixed.writable || fixed.enumerable || fixed.configurable ||
      fixed.value !== Object.prototype) return false;
  try {
    Object.prototype = {};
    return false;
  } catch (error) {
    if (error.name !== "TypeError") return false;
  }
  Box.prototype = Symbol;
  const box = new Box();
  try {
    box.iterator = 1;
    return false;
  } catch (error) {
    return error.name === "TypeError" && box.iterator === Symbol.iterator &&
      describe(box, "iterator") === undefined;
  }
}

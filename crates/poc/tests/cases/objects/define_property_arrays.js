// @pending ["wait:array descriptor"]
// @restore
// @fulfill ["wait:array descriptor", 0]
// @fulfilled true

async function run() {
  const define = Object.defineProperty;
  const describe = Object.getOwnPropertyDescriptor;
  const array = [];
  define(array, "4294967294", { value: 7, configurable: true });
  if (array.length !== 4294967295 || 0 in array || Object.keys(array).length !== 0) return false;
  define(array, "length", { value: "4" });
  define(array, 1, { get: function () { return this.length; }, enumerable: true });
  array[3] = 9;
  await wait("array descriptor");
  if (Object.keys(array).length !== 2 || Reflect.ownKeys(array)[2] !== "length") return false;
  const copy = [...array];
  if (copy.length !== 4 || copy[0] !== undefined || copy[1] !== 4 || copy[3] !== 9) return false;
  try {
    define(array, "length", { value: 0, writable: false });
    return false;
  } catch (error) {
    if (error.name !== "TypeError" || array.length !== 2 || 3 in array ||
        describe(array, "length").writable) return false;
  }
  try {
    array[2] = 1;
    return false;
  } catch (error) {
    if (error.name !== "TypeError") return false;
  }
  try {
    define(array, 2, { value: 1 });
    return false;
  } catch (error) {
    if (error.name !== "TypeError") return false;
  }
  try {
    define(array, "length", { value: 0.5 });
    return false;
  } catch (error) {
    if (error.name !== "RangeError") return false;
  }
  define(array, 0, { value: 6 });
  return array.length === 2 && array[0] === 6 && array[1] === 2;
}

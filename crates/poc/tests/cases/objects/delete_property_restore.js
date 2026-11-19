// @pending ["wait:key"]
// @restore
// @fulfill ["wait:key", "first"]
// @fulfilled true

export async function run() {
  const sym = Symbol("first");
  const other = Symbol("second");
  let getterCalls = 0;
  function C() {}
  Object.defineProperty(C.prototype, "first", {value: 9});
  const object = new C();
  Object.defineProperty(object, "first", {
    configurable: true, enumerable: true, get() { getterCalls++; return 1; }
  });
  object.second = 2;
  object[sym] = 3;
  object[other] = 4;
  const array = [1, 2, 3];
  Object.defineProperty(array, 2, {configurable: false});
  if (!delete array[1] || !Reflect.deleteProperty(object, sym)) return false;
  const remove = Reflect.deleteProperty;
  if (!delete (object?.[await wait("key")])) return false;
  if (getterCalls !== 0 || object.first !== 9 || object.hasOwnProperty("first")) return false;
  if (!remove(object, "first") || !("first" in object)) return false;
  if (array.length !== 3 || 1 in array || remove(array, "length") || remove(array, 2)) return false;
  try {
    delete array.length;
    return false;
  } catch (e) {
    if (e.name !== "TypeError") return false;
  }
  Object.defineProperty(object, "first", {value: 5, enumerable: true});
  object[sym] = 6;
  const keys = Reflect.ownKeys(object);
  return keys.length === 4 && keys[0] === "second" && keys[1] === "first" &&
    keys[2] === other && keys[3] === sym && array[2] === 3;
}

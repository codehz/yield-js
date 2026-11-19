// @pending ["wait:keys"]
// @restore
// @fulfill ["wait:keys", 0]
// @fulfilled true

function Box() {}

async function run() {
  Box.prototype.inherited = 1;
  const object = new Box();
  const symbol = Symbol("extra");
  object.z = 1;
  object[symbol] = 2;
  object[10] = 3;
  object.a = 4;
  object[2] = 5;
  object.z = 6;
  const accessor = { get read() { throw "enumeration read the getter"; } };
  const array = [1, 2];
  array.extra = 3;
  array[symbol] = 4;
  const keys = Object.keys;
  Reflect.ownKeys = Reflect.ownKeys;
  await wait("keys");
  if (keys !== Object.keys) return false;
  const result = keys(object);
  if (result.length !== 4 || result[0] !== "2" || result[1] !== "10" ||
      result[2] !== "z" || result[3] !== "a") return false;
  if (keys(accessor)[0] !== "read") return false;
  const indices = keys(array);
  if (indices.length !== 3 || indices[0] !== "0" || indices[1] !== "1" ||
      indices[2] !== "extra") return false;
  if (keys(Reflect).length !== 0 || Reflect.ownKeys(Reflect)[0] !== "ownKeys" ||
      keys(Box).length !== 0 || keys(Object).length !== 0 ||
      keys(Object.prototype).length !== 0) return false;
  try {
    keys(null);
    return false;
  } catch (error) {
    if (error.name !== "TypeError") return false;
  }
  const stringKeys = keys("a😀");
  return stringKeys.length === 3 && stringKeys[0] === "0" && stringKeys[2] === "2" &&
    keys(1).length === 0 && keys(symbol).length === 0;
}

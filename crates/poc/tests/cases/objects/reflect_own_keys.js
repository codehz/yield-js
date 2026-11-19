// @pending ["wait:keys"]
// @restore
// @fulfill ["wait:keys", 0]
// @fulfilled true

function Box() {}

export async function run() {
  const first = Symbol("same");
  const second = Symbol("same");
  const obj = {
    z: 0,
    [second]: 0,
    get x() { throw "enumeration must not read this"; },
    a: 0,
    set x(value) {},
    [first]: 0,
    "10": 0,
    "4294967295": 0,
    "2": 0,
    "01": 0,
    "4294967294": 0,
    [Symbol("key only")]: 42,
  };
  obj.z = 1;
  obj[second] = 1;
  Box.prototype.inherited = 1;
  const instance = new Box();
  instance.own = 1;
  const array = [1, 2, 3];
  array.z = 1;
  array[second] = 1;
  array["+1"] = 4;
  array["4294967295"] = 5;
  array.length = 2;
  const ownKeys = Reflect.ownKeys;
  await wait("keys");
  obj.end = 0;
  const keys = ownKeys(obj);
  if (typeof Reflect !== "object" || ownKeys !== Reflect.ownKeys) return false;
  if (keys.length !== 12 || keys[0] !== "2" || keys[1] !== "10" || keys[2] !== "4294967294" ||
      keys[3] !== "z" || keys[4] !== "x" || keys[5] !== "a" || keys[6] !== "4294967295" ||
      keys[7] !== "01" || keys[8] !== "end" || keys[9] !== second || keys[10] !== first ||
      typeof keys[11] !== "symbol" || obj[keys[11]] !== 42) return false;
  const own = Reflect.ownKeys(instance);
  if (own.length !== 1 || own[0] !== "own") return false;
  const indices = Reflect.ownKeys(array);
  if (indices.length !== 7 || indices[0] !== "0" || indices[1] !== "1" ||
      indices[2] !== "length" || indices[3] !== "z" || indices[4] !== "+1" ||
      indices[5] !== "4294967295" || indices[6] !== second || array[1] !== 2) return false;
  try {
    Reflect.ownKeys("abc");
  } catch (error) {
    return error.name === "TypeError";
  }
  return false;
}

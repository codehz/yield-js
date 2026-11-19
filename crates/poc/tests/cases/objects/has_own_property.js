// @fulfilled true

export async function run() {
  const obj = { a: 1, "[object Object]": undefined };
  const arr = [1, 2];
  arr.extra = undefined;
  return obj.hasOwnProperty("a") && !obj.hasOwnProperty("b") &&
    !obj.hasOwnProperty("hasOwnProperty") && obj.hasOwnProperty({}) &&
    arr.hasOwnProperty("1") && !arr.hasOwnProperty("2") &&
    arr.hasOwnProperty("length") && arr.hasOwnProperty("extra") &&
    arr.hasOwnProperty === obj.hasOwnProperty;
}

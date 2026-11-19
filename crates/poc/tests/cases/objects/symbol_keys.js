// @fulfilled 124

export async function run() {
  const a = Symbol("x");
  const b = Symbol("x");
  const obj = { x: 0 };
  obj[a] = 1;
  obj[b] = 2;
  obj.x = 4;
  if (obj[a] !== 1 || obj[b] !== 2 || obj.x !== 4) return -1;
  const zero = Symbol("0");
  const arr = [];
  arr[zero] = 9;
  if (arr.length !== 0 || arr[zero] !== 9) return -1;
  const marker = Symbol("prop");
  const holder = {
    n: 0,
    get [marker]() { return this.n; },
    set [marker](value) { this.n = value; },
  };
  holder[marker] = 5;
  if (holder[marker] !== 5 || !holder.hasOwnProperty(marker)) return -1;
  return obj[a] * 100 + obj[b] * 10 + obj.x;
}

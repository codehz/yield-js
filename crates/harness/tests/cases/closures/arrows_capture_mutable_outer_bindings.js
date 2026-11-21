// @returns 5

export function run() {
  let n = 3;
  const add = (x) => x + n;
  n = 4;
  return add(1);
}

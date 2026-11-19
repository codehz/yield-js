// @fulfilled true

export async function run() {
  let n = 0;
  const obj = { x: 7 };
  const result = delete (n++, obj.x);
  return result === true && n === 1 && obj.x === 7;
}

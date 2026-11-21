// @returns 15

export function run() {
  const obj = { a: 1, nested: { b: 2 }, list: [3, 4] };
  obj.a = obj.a + obj.nested.b;
  obj.list[1] += 5;
  const key = "computed";
  const extra = { [key]: 7 };
  return obj.a + obj.list.length + obj.list[0] + extra[key];
}

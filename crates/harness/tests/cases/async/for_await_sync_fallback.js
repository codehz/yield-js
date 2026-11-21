// @fulfilled "1234567|7|abcd"
export async function run() {
  const values = [];
  for await (const value of [1, 2, 3]) values.push(value);
  const iterable = {
    [Symbol.asyncIterator]: null,
    [Symbol.iterator]() {
      return [Promise.resolve(4), 5].values();
    },
  };
  for await (const value of iterable) values.push(value);
  async function* generated() {
    yield Promise.resolve(6);
    yield 7;
  }
  for await (const value of generated()) values.push(value);
  const pairs = [["a", "b"], ["c", "d"]];
  let flat = "";
  for await (const [first, second] of pairs) flat += first + second;
  return `${values.join("")}|${values.length}|${flat}`;
}

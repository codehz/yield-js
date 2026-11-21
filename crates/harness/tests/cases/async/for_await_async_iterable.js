// @fulfilled "1,3,5|9|done"
export async function run() {
  let index = 0;
  const iterable = {
    [Symbol.asyncIterator]() {
      return {
        next() {
          index += 1;
          if (index > 3) return Promise.resolve({ done: true });
          return Promise.resolve({ value: index * 2 - 1, done: false });
        },
      };
    },
  };
  const values = [];
  let total = 0;
  for await (const value of iterable) {
    values.push(value);
    total += value;
  }
  return `${values.join(",")}|${total}|done`;
}

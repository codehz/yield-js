// @pending ["wait:a", "wait:b"]
// @restore
// @fulfill ["wait:b", 2]
// @restore
// @fulfill ["wait:a", 1]
// @fulfilled "1,2,3:4"

export async function run() {
  let reads = 0;
  const result = await Promise.all({
    [Symbol.iterator]() {
      return {
        next() {
          reads++;
          if (reads === 1) return { value: wait("a") };
          if (reads === 2) return { value: wait("b") };
          if (reads === 3) return { value: 3 };
          return { done: true };
        },
      };
    },
  });
  return result.join(",") + ":" + reads;
}

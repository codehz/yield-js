// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled 20

async function f(a, ...rest) {
  const read = () => rest[0];
  const alias = rest;
  const n = await wait("a");
  rest[0] += n;
  return a + read() + alias[1] + (alias === rest ? 1 : 0);
}

export function run() {
  return f(4, 5, 8);
}

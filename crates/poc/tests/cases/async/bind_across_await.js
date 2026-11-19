// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled 12

export async function run() {
  const f = add.bind(null, 10);
  const v = await wait("a");
  return f(v);
}

function add(a, b) {
  return a + b;
}

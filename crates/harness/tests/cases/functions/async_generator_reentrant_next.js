// @fulfilled "1:false:2:false"

export async function run() {
  let inner;
  async function* g() {
    // Queued from inside the body: the generator is executing, so the request
    // is appended instead of reentering, and no TypeError is thrown.
    inner = it.next();
    yield 1;
    yield 2;
  }
  const it = g();
  const first = await it.next();
  const second = await inner;
  return `${first.value}:${first.done}:${second.value}:${second.done}`;
}

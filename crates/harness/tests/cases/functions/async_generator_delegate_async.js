// @fulfilled "1:false|2:false|13:true"

export async function run() {
  async function* inner() {
    yield 1;
    yield 2;
    return 3;
  }
  async function* outer() {
    const fromInner = yield* inner();
    return fromInner + 10;
  }
  const it = outer();
  const first = await it.next();
  const second = await it.next();
  const third = await it.next();
  return [
    `${first.value}:${first.done}`,
    `${second.value}:${second.done}`,
    `${third.value}:${third.done}`,
  ].join("|");
}

// @fulfilled "1:true"

export async function run() {
  async function* g() {
    const first = yield 1;
    return first;
  }
  const it = g();
  // The argument of the first `next` is ignored; later ones are forwarded.
  const started = await it.next("ignored");
  const forwarded = await it.next(2);
  return `${started.value}:${forwarded.value === 2}`;
}

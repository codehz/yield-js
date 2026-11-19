// @fulfilled 15

export async function run() {
  const obj = { n: 100, calc(a, b) { return this.n + 2 * a + b; } };
  const f = obj.calc.bind({ n: 3 }, 5);
  const holder = { f, n: 200 };
  return holder.f(await Promise.resolve(2));
}

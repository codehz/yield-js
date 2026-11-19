// @fulfilled true

async function run() {
  const obj = { n: 100, calc(a, b) { return this.n + 10 * a + b; } };
  const other = { n: 3 };
  const ok = obj.calc.call(other, 2, 4) === 27 && obj.calc.apply(other, [4, 2]) === 45 &&
    noArgs.call(undefined) && noArgs.call() && noArgs.apply(undefined, null) && noArgs.apply();
  try {
    obj.calc.apply(other, { length: 0 });
  } catch (e) {
    return ok && typeof e === "object" && e.name === "TypeError" && e.message === "createListFromArrayLike";
  }
  return false;
}

function noArgs(a) {
  return this === undefined && a === undefined;
}

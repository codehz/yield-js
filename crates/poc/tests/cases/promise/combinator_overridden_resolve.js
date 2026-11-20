// @fulfilled true

export async function run() {
  const resolve = Promise.resolve;
  let reads = 0;
  let calls = "";
  let correct = true;
  Object.defineProperty(Promise, "resolve", {
    configurable: true,
    get() {
      reads++;
      return function (value) {
        correct = correct && this === Promise && arguments.length === 1;
        calls += value;
        return Reflect.apply(resolve, this, [value + 10]);
      };
    },
  });
  const all = await Promise.all([1, 2]);
  const settled = await Promise.allSettled([3]);
  const raced = await Promise.race([4]);
  const any = await Promise.any([5]);
  Object.defineProperty(Promise, "resolve", { configurable: true, value: 0 });
  const bad = Promise.all([]);
  Object.defineProperty(Promise, "resolve", { configurable: true, get() { throw "get"; } });
  const getter = Promise.race([]);
  Object.defineProperty(Promise, "resolve", { value() { throw "call"; } });
  const invoked = Promise.any([1]);
  let errors = "";
  for (const promise of [bad, getter, invoked]) {
    try { await promise; }
    catch (error) { errors += typeof error === "string" ? error : error.name; }
  }
  return correct && reads === 4 && calls === "12345" && all[1] === 12
    && settled[0].value === 13 && raced === 14 && any === 15 && errors === "TypeErrorgetcall";
}

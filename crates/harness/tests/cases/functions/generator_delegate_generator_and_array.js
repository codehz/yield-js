// @returns "1:2:4:5:NaN:true"

export function run() {
  function* inner() {
    yield 1;
    yield 2;
    return 3;
  }
  function* outer() {
    const fromInner = yield* inner();
    const fromArray = yield* [4, 5];
    // An array delegate completes with undefined, as in ECMAScript.
    return fromInner + fromArray + 2;
  }
  const it = outer();
  const first = it.next();
  const second = it.next();
  const third = it.next();
  const fourth = it.next();
  const last = it.next();
  return [first.value, second.value, third.value, fourth.value, last.value, last.done].join(":");
}

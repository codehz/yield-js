// @returns "1:2:true"

export function run() {
  function* g() {
    for (const value of [1, 2]) {
      yield value;
    }
  }
  const it = g();
  const first = it.next();
  const second = it.next();
  const third = it.next();
  return [first.value, second.value, third.done].join(":");
}

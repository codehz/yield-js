// @returns "2:false:3:false:12:true"

export function run() {
  function* g(x) {
    try {
      return x + (yield 2);
    } finally {
      yield 3;
    }
  }
  const it = g(5);
  const first = it.next(99);
  const second = it.next(7);
  const third = it.next();
  return [first.value, first.done, second.value, second.done, third.value, third.done].join(":");
}

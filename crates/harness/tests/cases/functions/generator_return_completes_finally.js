// @returns "3:false:8:true"

export function run() {
  function* g(x) {
    try {
      return x + (yield 2);
    } finally {
      yield 3;
    }
  }
  const it = g(5);
  it.next();
  const closed = it.return(8);
  const last = it.next();
  return [closed.value, closed.done, last.value, last.done].join(":");
}

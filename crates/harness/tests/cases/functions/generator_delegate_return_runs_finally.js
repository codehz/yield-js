// @returns "1:8:true"

export function run() {
  let cleanups = 0;
  const delegate = {
    next() {
      return { done: false, value: 1 };
    },
    return(value) {
      return { done: true, value: value };
    },
  };
  delegate[Symbol.iterator] = function () {
    return delegate;
  };
  function* outer() {
    try {
      yield* delegate;
    } finally {
      cleanups = cleanups + 1;
    }
  }
  const it = outer();
  it.next();
  const closed = it.return(8);
  return [cleanups, closed.value, closed.done].join(":");
}

// @returns "TypeError:0"

export function run() {
  let closes = 0;
  const delegate = {
    next() {
      return { done: false, value: 1 };
    },
    throw: 5,
    return(value) {
      closes = closes + 1;
      return { done: true, value: value };
    },
  };
  delegate[Symbol.iterator] = function () {
    return delegate;
  };
  function* outer() {
    try {
      yield* delegate;
    } catch (error) {
      const kind = error instanceof TypeError ? "TypeError" : "other";
      return kind + ":" + closes;
    }
  }
  const it = outer();
  it.next();
  return it.throw("boom").value;
}

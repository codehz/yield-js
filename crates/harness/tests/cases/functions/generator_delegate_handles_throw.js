// @returns "handled:boom:true:0"

export function run() {
  let closes = 0;
  const delegate = {
    next() {
      return { done: false, value: 1 };
    },
    throw(value) {
      return { done: true, value: "handled:" + value };
    },
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
      const end = yield* delegate;
      return end;
    } catch (error) {
      return "caught";
    }
  }
  const it = outer();
  it.next();
  const result = it.throw("boom");
  return [result.value, result.done, closes].join(":");
}

// @returns "TypeError:true:1"

export function run() {
  let calls = 0;
  const delegate = {
    next() {
      calls = calls + 1;
      return 5;
    },
  };
  delegate[Symbol.iterator] = function () {
    return delegate;
  };
  function* outer() {
    try {
      yield* delegate;
    } catch (error) {
      return error instanceof TypeError ? "TypeError" : "other";
    }
  }
  const it = outer();
  const step = it.next();
  return [step.value, step.done, calls].join(":");
}

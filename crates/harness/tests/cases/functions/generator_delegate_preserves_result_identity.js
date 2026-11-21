// @returns "true:0:true:end"

export function run() {
  let reads = 0;
  const step = { done: false };
  Object.defineProperty(step, "value", {
    enumerable: true,
    configurable: true,
    get() {
      reads = reads + 1;
      return 5;
    },
  });
  let calls = 0;
  const delegate = {
    next() {
      calls = calls + 1;
      return calls === 1 ? step : { done: true, value: "end" };
    },
  };
  delegate[Symbol.iterator] = function () {
    return delegate;
  };
  function* outer() {
    return yield* delegate;
  }
  const it = outer();
  const first = it.next();
  const same = first === step;
  const readsAfterFirst = reads;
  const second = it.next();
  return [same, readsAfterFirst, second.done, second.value].join(":");
}

// @returns "1:false:2:false:end:true:1"

export function run() {
  let closes = 0;
  const steps = [
    { done: false, value: 1 },
    { done: false, value: 2 },
    { done: true, value: "end" },
  ];
  const delegate = {
    next() {
      return steps.shift();
    },
    return(value) {
      closes = closes + 1;
      return { done: false, value: 99 };
    },
  };
  delegate[Symbol.iterator] = function () {
    return delegate;
  };
  function* outer() {
    const end = yield* delegate;
    return end;
  }
  const it = outer();
  const first = it.next();
  const closed = it.return(8);
  const last = it.next();
  return [
    first.value,
    first.done,
    closed.value,
    closed.done,
    last.value,
    last.done,
    closes,
  ].join(":");
}

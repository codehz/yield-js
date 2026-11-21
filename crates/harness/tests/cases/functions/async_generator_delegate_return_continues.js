// @fulfilled "1:false|2:false|3:false|end:true|1"

export async function run() {
  let closes = 0;
  let steps = 0;
  const delegate = {
    [Symbol.asyncIterator]() {
      return this;
    },
    next(value) {
      steps = steps + 1;
      if (steps > 3) return Promise.resolve({ value: "end", done: true });
      return Promise.resolve({ value: steps, done: false });
    },
    return() {
      closes = closes + 1;
      // A delegated return that is not done keeps the delegation running.
      return Promise.resolve({ value: "ignored", done: false });
    },
  };
  async function* g() {
    return yield* delegate;
  }
  const it = g();
  const first = await it.next();
  const second = await it.return(9);
  const third = await it.next();
  const last = await it.next();
  return [
    `${first.value}:${first.done}`,
    `${second.value}:${second.done}`,
    `${third.value}:${third.done}`,
    `${last.value}:${last.done}`,
    closes,
  ].join("|");
}

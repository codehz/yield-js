// @fulfilled "1:false|2:false|1|true"

export async function run() {
  let closes = 0;
  let steps = 0;
  const delegate = {
    [Symbol.asyncIterator]() {
      return this;
    },
    next() {
      steps = steps + 1;
      return Promise.resolve({ value: steps, done: false });
    },
    // No `throw`: the missing method closes the delegate with no argument
    // first, then reports a TypeError.
    return() {
      closes = closes + 1;
      return Promise.resolve({ value: "closed", done: true });
    },
  };
  async function* g() {
    return yield* delegate;
  }
  const it = g();
  const first = await it.next();
  const second = await it.next();
  let reason;
  try {
    await it.throw("boom");
  } catch (error) {
    reason = error;
  }
  return [
    `${first.value}:${first.done}`,
    `${second.value}:${second.done}`,
    closes,
    reason instanceof TypeError,
  ].join("|");
}

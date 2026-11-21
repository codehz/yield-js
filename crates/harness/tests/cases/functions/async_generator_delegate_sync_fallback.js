// @fulfilled "1:false|caught:2:false|undefined:true"

export async function run() {
  let step = 0;
  const delegate = {
    [Symbol.iterator]() {
      return this;
    },
    next() {
      step = step + 1;
      if (step > 2) return { value: undefined, done: true };
      // A synchronous delegate's value is assimilated; the rejected one must
      // reach the lexical catch instead of the consumer.
      return { value: step === 1 ? 1 : Promise.reject(2), done: false };
    },
  };
  async function* g() {
    try {
      yield* delegate;
    } catch (error) {
      yield `caught:${error}`;
    }
  }
  const it = g();
  const first = await it.next();
  const second = await it.next();
  const third = await it.next();
  return [
    `${first.value}:${first.done}`,
    `${second.value}:${second.done}`,
    `${third.value}:${third.done}`,
  ].join("|");
}

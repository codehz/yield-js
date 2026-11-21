// @fulfilled "1:false|caught:2:false|undefined:true"

export async function run() {
  // Observing the rejection early keeps it out of the unhandled notification
  // channel; awaiting it still rejects and reaches the lexical catch.
  const refused = Promise.reject(2);
  refused.catch(() => undefined);
  async function* g() {
    try {
      yield* [Promise.resolve(1), refused];
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

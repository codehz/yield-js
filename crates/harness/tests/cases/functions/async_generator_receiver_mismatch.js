// @fulfilled "true|TypeError|TypeError|TypeError"

export async function run() {
  async function* g() {
    yield 1;
  }
  const proto = Object.getPrototypeOf(Object.getPrototypeOf(g()));
  const outcomes = [];
  let isPromise = false;
  for (const name of ["next", "throw", "return"]) {
    try {
      // A mismatched receiver rejects a fresh Promise instead of throwing.
      const pending = proto[name].call({ mark: name });
      isPromise = pending instanceof Promise;
      await pending;
      outcomes.push("fulfilled");
    } catch (error) {
      outcomes.push(error instanceof TypeError ? "TypeError" : "other");
    }
  }
  return [isPromise, ...outcomes].join("|");
}

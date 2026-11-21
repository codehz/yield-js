// @fulfilled "1|no|true|1"

export async function run() {
  let ranFinally = 0;
  async function* g() {
    try {
      yield 1;
      yield 2;
    } finally {
      ranFinally = ranFinally + 1;
    }
  }
  const it = g();
  const first = await it.next();
  // The injected return value is assimilated at the suspension, so a rejection
  // enters the yield's throw landing and runs the finally clauses.
  let reason;
  try {
    await it.return(Promise.reject("no"));
  } catch (error) {
    reason = error;
  }
  const after = await it.next();
  return [first.value, reason, after.done, ranFinally].join("|");
}

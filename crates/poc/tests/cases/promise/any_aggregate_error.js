// @fulfilled true
export async function run() {
  let caught;
  try {
    await Promise.any([Promise.reject("first"), Promise.reject("second"), Promise.reject("third")]);
  } catch (error) {
    caught = error;
  }
  if (!(caught instanceof AggregateError)) return false;
  if (caught.errors.length !== 3 || caught.errors[0] !== "first" || caught.errors[1] !== "second" || caught.errors[2] !== "third") return false;
  if (await Promise.any([Promise.reject("no"), Promise.resolve("winner")]) !== "winner") return false;
  try { await Promise.any([]); return false; } catch (error) {
    if (!(error instanceof AggregateError) || error.errors.length !== 0) return false;
  }
  return true;
}

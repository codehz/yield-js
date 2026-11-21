// @returns "caught:boom:true"

export function run() {
  function* g() {
    try {
      yield 1;
    } catch (error) {
      return "caught:" + error;
    }
  }
  const it = g();
  const first = it.next();
  if (first.value !== 1 || first.done) {
    return "first step failed";
  }
  const second = it.throw("boom");
  return [second.value, second.done].join(":");
}

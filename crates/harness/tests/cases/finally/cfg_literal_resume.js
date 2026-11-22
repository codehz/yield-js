// @fulfilled "4:false|9:true|LFL8F"

export async function run() {
  let trace = "";
  async function* values(flag) {
    let kept;
    if (flag) kept = 4;
    else kept = 4;
    const copy = kept;
    try {
      if (false) {
        yield await "unreachable";
      }
      try {
        kept = (() => { throw "L"; })();
      } catch (error) {
        if (kept) trace += error;
      }
      const sent = yield copy;
      trace += sent;
    } finally {
      if (copy) trace += await "F";
    }
  }

  const first = values(true);
  const start = await first.next();
  const returned = await first.return(9);
  const second = values(false);
  await second.next();
  await second.next(8);
  // Two throws and two finalizers. The second next injects 8, not the
  // previous yield operand or the first generator's return payload.
  return `${start.value}:${start.done}|${returned.value}:${returned.done}|${trace}`;
}

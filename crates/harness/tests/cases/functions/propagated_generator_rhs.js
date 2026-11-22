// @fulfilled "3:5|3:9|3:11|333"

export async function run() {
  let trace = "";
  async function* numbers() {
    const rhs = 1 + 2;
    try {
      const sent = yield rhs;
      return sent + rhs;
    } catch (error) {
      return error + rhs;
    } finally {
      trace += await rhs;
    }
  }
  const a = numbers();
  const b = numbers();
  const c = numbers();
  const av = (await a.next()).value;
  const ar = (await a.next(2)).value;
  const bv = (await b.next()).value;
  const br = (await b.throw(6)).value;
  const cv = (await c.next()).value;
  const cr = (await c.return(11)).value;
  return `${av}:${ar}|${bv}:${br}|${cv}:${cr}|${trace}`;
}

// @fulfilled "early|true|0|7:true"

export async function run() {
  let ran = 0;
  async function* g() {
    ran = ran + 1;
    yield 1;
  }
  // Neither `throw` nor `return` enters a body that has not started yet.
  const unborn = g();
  let reason;
  try {
    await unborn.throw("early");
  } catch (error) {
    reason = error;
  }
  const afterThrow = await unborn.next();
  const fresh = g();
  // A return argument is assimilated before the request reports done.
  const closed = await fresh.return(Promise.resolve(7));
  return [
    reason,
    afterThrow.done,
    ran,
    `${closed.value}:${closed.done}`,
  ].join("|");
}

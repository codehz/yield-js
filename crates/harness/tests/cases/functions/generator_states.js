// @returns "1:true:true::true:0:true:true"

export function run() {
  let ran = 0;
  function* g() {
    ran = ran + 1;
    return 1;
  }
  const it = g();
  const first = it.next();
  const second = it.next();
  const closed = it.return(5);
  let threw = false;
  try {
    it.throw("closed");
  } catch (error) {
    threw = error === "closed";
  }
  ran = 0;
  const unborn = g();
  let unbornThrew = false;
  try {
    unborn.throw("early");
  } catch (error) {
    unbornThrew = error === "early";
  }
  const afterThrow = unborn.next();
  return [
    first.value,
    first.done,
    second.done,
    second.value,
    threw,
    ran,
    afterThrow.done,
    unbornThrew,
  ].join(":");
}

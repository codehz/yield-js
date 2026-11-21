// @returns "8:true:0"

export function run() {
  let entered = 0;
  function* g() {
    try {
      yield 1;
    } catch (error) {
      entered = entered + 1;
      yield "caught";
    }
    return "done";
  }
  const it = g();
  it.next();
  const closed = it.return(8);
  return [closed.value, closed.done, entered].join(":");
}

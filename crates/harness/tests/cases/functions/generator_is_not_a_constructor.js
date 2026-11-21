// @returns "true:TypeError:object:function"

export function run() {
  function* g(a, b) {}
  let threw = "none";
  try {
    new g();
  } catch (error) {
    threw = error instanceof TypeError ? "TypeError" : "other";
  }
  return [g.length === 2, threw, typeof g.prototype, typeof g.prototype.next].join(
    ":"
  );
}

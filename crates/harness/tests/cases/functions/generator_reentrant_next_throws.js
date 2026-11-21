// @returns "TypeError:1:true"

export function run() {
  let caught = "";
  let self = null;
  function* g() {
    try {
      self.next();
    } catch (error) {
      caught = error instanceof TypeError ? "TypeError" : "other";
    }
    yield 1;
    return 2;
  }
  self = g();
  const first = self.next();
  const second = self.next();
  return [caught, first.value, second.done].join(":");
}

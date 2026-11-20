// @fulfilled "A😀:7:rejected:bad:1:INTNTD:x:y"

export async function run() {
  let trace = "";
  const iterator = {
    index: 0,
    get next() {
      trace += "N";
      return function () {
        trace += "T";
        if (this.index++) {
          return { get done() { trace += "D"; return true; }, get value() { throw "wrong value"; } };
        }
        return { value: { get then() { trace += "N"; return undefined; } } };
      };
    },
    return() { throw "unexpected close"; },
  };
  const iterable = {
    [Symbol.iterator]() { trace += "I"; return iterator; },
  };
  const input = await Promise.all(iterable);
  const chars = await Promise.all("A😀");
  const array = ["ignored"];
  array[Symbol.iterator] = function () {
    let done = false;
    return { next() { if (done) return { done: true }; done = true; return { value: 7 }; } };
  };
  const overridden = await Promise.all(array);
  const items = await Promise.allSettled({
    [Symbol.iterator]() { return [Promise.reject("bad")].values(); },
  });
  return chars.join("") + ":" + overridden[0] + ":" + items[0].status + ":" + items[0].reason
    + ":" + input.length + ":" + trace + ":" + await Promise.race("xy") + ":" + await Promise.any("yx");
}

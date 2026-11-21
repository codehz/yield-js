// @returns true

export function run() {
  class Sub extends Promise {}
  const p = Sub.resolve(1);
  if (!(p instanceof Sub) || Sub.resolve(p) !== p || Promise.resolve(p) === p) return false;
  if (!(Sub.reject(2) instanceof Sub)) return false;
  const result = {};
  p.constructor = result;
  if (Promise.resolve.call(result, p) !== p || Sub.resolve(p) === p) return false;
  let observation;
  function C(executor) {
    executor(function (v) { observation = this === undefined && v === 3; },
             function (v) { observation = this === undefined && v === 4; return 9; });
    return result;
  }
  if (Promise.resolve.call(C, 3) !== result || !observation) return false;
  observation = false;
  if (Promise.reject.call(C, 4) !== result || !observation) return false;
  function Missing() { return result; }
  try { Promise.resolve.call(Missing, 1); return false; }
  catch (e) { if (!(e instanceof TypeError)) return false; }
  function Throws(executor) { executor(() => {}, () => { throw result; }); }
  try { Promise.reject.call(Throws, 4); return false; }
  catch (e) { return e === result; }
}

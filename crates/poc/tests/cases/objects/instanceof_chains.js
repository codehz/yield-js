// @returns true

export function run() {
  function C() {}
  const instance = new C();
  const child = Object.create(Object.create(C.prototype));
  const bound = C.bind(null);
  bound.prototype = {};
  return instance instanceof C && child instanceof C && instance instanceof bound &&
    !(C.prototype instanceof C) && !({ prototype: C.prototype } instanceof C) &&
    !(42 instanceof C) && {} instanceof Object && [] instanceof Object &&
    new Error() instanceof Error && new TypeError() instanceof Error &&
    Promise.resolve(1) instanceof Promise;
}

// @pending ["wait:define"]
// @restore
// @fulfill ["wait:define", 0]
// @fulfilled true

function Descriptor() {}

export async function run() {
  const define = Object.defineProperty;
  const describe = Object.getOwnPropertyDescriptor;
  const symbol = Symbol("state");
  const target = { a: 1, b: 2 };
  let state = 3;
  let trace = "";
  const getter = function () { return this.a + state; };
  const setter = function (n) { state = n; };
  Descriptor.prototype = {
    get enumerable() { trace += "e"; this.configurable = 1; return 1; },
    get get() { trace += "g"; return getter; },
  };
  const descriptor = new Descriptor();
  descriptor.set = setter;
  if (define(target, "a", descriptor) !== target || trace !== "eg") return false;
  if (describe(target, "a").get !== getter || describe(target, "a").set !== setter) return false;
  define(target, "a", { get: undefined });
  target.a = 8;
  define(target, "a", { get: function () { return this.b + state; } });
  define(target, symbol, { value: 3 });
  define(target, "empty", { get: undefined });
  define(target, "implicit", {});
  const bound = setter.bind(null, 12);
  define(bound, "x", { get: Object.keys });
  await wait("define");
  if (define !== Object.defineProperty || target.a !== 10 || target[symbol] !== 3) return false;
  const keys = Object.keys(target);
  if (keys.length !== 2 || keys[0] !== "a" || keys[1] !== "b") return false;
  const fixed = describe(target, symbol);
  if (fixed.enumerable || fixed.configurable || fixed.writable) return false;
  const empty = describe(target, "empty");
  if (!("get" in empty) || empty.get !== undefined || "value" in empty || !("empty" in target)) return false;
  const implicit = describe(target, "implicit");
  if (!("value" in implicit) || implicit.value !== undefined || implicit.writable) return false;
  if (describe(bound, "x").get !== Object.keys) return false;
  define(target, "a", { value: 4 });
  const changed = describe(target, "a");
  return changed.value === 4 && changed.enumerable && changed.configurable && !changed.writable;
}

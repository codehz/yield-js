// @pending ["wait:properties"]
// @restore
// @fulfill ["wait:properties", 0]
// @fulfilled true

function Descriptors() {}

export async function run() {
  const define = Object.defineProperties;
  const target = {};
  const symbol = Symbol("property");
  let trace = "";
  Descriptors.prototype = { get inherited() { throw "inherited"; } };
  const source = new Descriptors();
  source.z = { get value() { trace += "z"; return 30; } };
  source[10] = { get value() { trace += "a"; return 10; } };
  source.skip = null;
  Object.defineProperty(source, "hidden", {
    configurable: true,
    get: function () { trace += "h"; return { value: 40 }; },
  });
  Object.defineProperty(source, 2, {
    enumerable: true,
    get: function () {
      trace += "2";
      Object.defineProperty(this, "skip", { enumerable: false });
      Object.defineProperty(this, "hidden", { enumerable: true });
      this.added = null;
      return { value: 20 };
    },
  });
  source[symbol] = {
    get get() {
      trace += "s";
      if ("z" in target) throw "applied before conversion finished";
      return function () { return this.z + this[2]; };
    },
  };
  if (define(target, source) !== target || trace !== "2azhs") return false;
  await wait("properties");
  const keys = Reflect.ownKeys(target);
  return keys.length === 5 && keys[0] === "2" && keys[1] === "10" && keys[2] === "z"
    && keys[3] === "hidden" && keys[4] === symbol && target.hidden === 40 && target[symbol] === 50;
}

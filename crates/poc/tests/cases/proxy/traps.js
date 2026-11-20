// @returns true

export function run() {
  const symbol = Symbol("key");
  const target = { [symbol]: 2, x: 1 };
  const receiver = { marker: 4 };
  let trace = "";
  let proxy;
  const handler = {
    get(t, k, r) {
      if (this !== handler || t !== target || k !== symbol || r !== receiver) throw "get args";
      trace += "g";
      return t[k] + r.marker;
    },
    set(t, k, v, r) {
      if (this !== handler || t !== target || k !== "x" || v !== 9 || r !== proxy) throw "set args";
      trace += "s";
      t[k] = v;
      return "accepted";
    },
    has(t, k) {
      if (this !== handler || t !== target || k !== symbol) throw "has args";
      trace += "h";
      return 1;
    },
    deleteProperty(t, k) {
      if (this !== handler || t !== target || k !== "x") throw "delete args";
      trace += "d";
      return delete t[k];
    },
    ownKeys(t, extra) {
      if (this !== handler || t !== target || extra !== undefined) throw "ownKeys args";
      trace += "k";
      return { length: 2, 0: symbol, 1: "x" };
    },
    getOwnPropertyDescriptor(t, k) {
      if (this !== handler || t !== target || k !== symbol) throw "descriptor args";
      trace += "p";
      return Reflect.getOwnPropertyDescriptor(t, k);
    },
  };
  proxy = new Proxy(target, handler);
  const key = { [Symbol.toPrimitive]() { trace += "c"; return symbol; } };
  if (Reflect.get(proxy, key, receiver) !== 6) return false;
  proxy.x = 9;
  if (target.x !== 9 || !(symbol in proxy)) return false;
  const keys = Reflect.ownKeys(proxy);
  if (keys.length !== 2 || keys[0] !== symbol || keys[1] !== "x") return false;
  const desc = Reflect.getOwnPropertyDescriptor(proxy, symbol);
  return desc.value === 2 && desc.enumerable && delete proxy.x &&
    !("x" in target) && trace === "cgshkpd";
}

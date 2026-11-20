// @returns true

export function run() {
  const symbol = Symbol("copy");
  const target = { a: 1, b: 2, hidden: 3, [symbol]: 4 };
  const source = new Proxy(target, {
    ownKeys() { return [symbol, "b", "hidden", "a", "missing"]; },
    getOwnPropertyDescriptor(t, k) {
      if (k === "hidden") return { value: 3, configurable: true };
      return Reflect.getOwnPropertyDescriptor(t, k);
    },
    get(t, k, r) { return k === "a" ? 10 : Reflect.get(t, k, r); },
  });
  if (Object.keys(source).join("|") !== "b|a" || !source.hasOwnProperty("hidden") ||
      source.hasOwnProperty("missing")) return false;
  if (JSON.stringify(source) !== '{"b":2,"a":10}') return false;
  const descriptors = Object.getOwnPropertyDescriptors(source);
  if (descriptors.a.value !== 1 || descriptors.hidden.enumerable ||
      descriptors[symbol].value !== 4 || "missing" in descriptors) return false;
  const spread = { ...source };
  const { a, ...rest } = source;
  if (a !== 10 || spread.a !== 10 || spread[symbol] !== 4 || "hidden" in spread ||
      "a" in rest || rest.b !== 2 || rest[symbol] !== 4) return false;
  const writes = [];
  const assigned = new Proxy({}, {
    set(t, k, v) { writes.push(k); t[k] = v; return true; },
  });
  return Object.assign(assigned, source) === assigned && writes.length === 3 &&
    writes[0] === symbol && writes[1] === "b" && writes[2] === "a" && assigned.a === 10;
}

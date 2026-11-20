// @returns true

function typeError(fn) {
  try { fn(); } catch (e) { return e.name === "TypeError"; }
  return false;
}

export function run() {
  if (!typeError(() => Proxy({}, {})) || !typeError(() => new Proxy()) ||
      !typeError(() => new Proxy(1, {})) || !typeError(() => new Proxy({}, null)) ||
      !typeError(() => new Proxy(() => 1, {})) || !typeError(() => new Proxy(new Map(), {}))) throw "constructor";

  const array = [1, , 3];
  const handler = { get: null, set: undefined, deleteProperty: null, ownKeys: undefined };
  const proxy = Reflect.construct(Proxy, [array, handler]);
  if (!typeError(() => proxy()) || !typeError(() => new proxy())) throw "callability";
  if (proxy.length !== 3 || proxy[0] !== 1 || 1 in proxy || !(2 in proxy) ||
      Reflect.ownKeys(proxy).join("|") !== "0|2|length" ||
      Reflect.getOwnPropertyDescriptor(proxy, "length").enumerable) throw "array reads";
  proxy[4] = 5;
  if (array.length !== 5 || array[4] !== 5 || !Reflect.deleteProperty(proxy, 2) ||
      2 in array || array.length !== 5) throw "array delete";
  if (!Reflect.set(proxy, "length", 1) || array.length !== 1 || 4 in array) throw "array length";

  const target = { get x() { return this.marker; }, set x(v) { this.marker = v; }, marker: 2 };
  let lookups = 0;
  const inherited = { get get() { lookups += 1; return null; } };
  const dynamic = Object.create(inherited);
  const p = new Proxy(target, dynamic);
  const receiver = { marker: 3 };
  // The getter's this.marker lookup also goes through the proxy.
  if (Reflect.get(p, "x", receiver) !== 3 || p.x !== 2 || lookups !== 3 ||
      !Reflect.set(p, "x", 4, receiver) || receiver.marker !== 4) throw "receiver";
  Object.defineProperty(dynamic, "get", { value: 1, configurable: true });
  if (!typeError(() => p.x)) throw "non-callable get";
  Object.defineProperty(dynamic, "get", { get() { throw "lookup"; }, configurable: true });
  try { p.x; throw "lookup did not throw"; } catch (e) { if (e !== "lookup") throw e; }
  Object.defineProperty(dynamic, "get", { value() { throw "trap"; }, configurable: true });
  try { p.x; throw "trap did not throw"; } catch (e) { if (e !== "trap") throw e; return true; }
}

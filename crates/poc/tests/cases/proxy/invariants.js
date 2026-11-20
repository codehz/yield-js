// @returns true

function typeError(fn) {
  try { fn(); } catch (e) { return e.name === "TypeError"; }
  return false;
}

export function run() {
  const fixed = Object.defineProperties({}, {
    zero: { value: -0 },
    nan: { value: NaN },
    accessor: { get: undefined, set: undefined },
  });
  const handler = {
    get(t, k) { return k === "zero" ? 0 : k === "nan" ? NaN : 1; },
    set() { return true; },
    has() { return false; },
    deleteProperty() { return true; },
    ownKeys() { return ["zero"]; },
    getOwnPropertyDescriptor() { return undefined; },
  };
  const p = new Proxy(fixed, handler);
  if (!typeError(() => p.zero) || !Object.is(p.nan, NaN) || !typeError(() => p.accessor) ||
      !typeError(() => Reflect.set(p, "zero", 0)) || !Reflect.set(p, "zero", -0) ||
      !typeError(() => Reflect.set(p, "accessor", undefined)) ||
      !typeError(() => "zero" in p) || !typeError(() => Reflect.deleteProperty(p, "zero")) ||
      !typeError(() => Reflect.ownKeys(p)) ||
      !typeError(() => Reflect.getOwnPropertyDescriptor(p, "zero"))) return false;

  const t = { present: 1 };
  const h = {
    set() { return 0; },
    deleteProperty() { return ""; },
    has() { return false; },
    ownKeys() { return ["present", "present"]; },
    getOwnPropertyDescriptor() { return 0; },
  };
  const q = new Proxy(t, h);
  if (Reflect.set(q, "present", 2) !== false || Reflect.deleteProperty(q, "present") !== false ||
      !typeError(() => { q.present = 2; }) || !typeError(() => delete q.present) || t.present !== 1 ||
      !typeError(() => Reflect.ownKeys(q)) || !typeError(() => Object.getOwnPropertyDescriptor(q, "present"))) return false;
  h.ownKeys = () => [1];
  if (!typeError(() => Reflect.ownKeys(q))) return false;
  h.ownKeys = () => null;
  if (!typeError(() => Reflect.ownKeys(q))) return false;
  h.getOwnPropertyDescriptor = () => ({ value: 1 });
  if (!typeError(() => Object.getOwnPropertyDescriptor(q, "present"))) return false;

  Object.preventExtensions(t);
  h.deleteProperty = () => true;
  h.ownKeys = () => ["present", "extra"];
  h.getOwnPropertyDescriptor = () => ({ configurable: true });
  if (!typeError(() => Reflect.has(q, "present")) || !typeError(() => delete q.present) ||
      !typeError(() => Reflect.ownKeys(q)) || !typeError(() => Reflect.getOwnPropertyDescriptor(q, "extra"))) return false;
  h.ownKeys = () => [];
  if (!typeError(() => Reflect.ownKeys(q))) return false;
  h.ownKeys = () => ["present"];
  h.getOwnPropertyDescriptor = () => undefined;
  if (Reflect.ownKeys(q)[0] !== "present" || !typeError(() => Reflect.getOwnPropertyDescriptor(q, "present"))) return false;

  const array = Object.freeze([1]);
  const a = new Proxy(array, {
    ownKeys() { return ["0"]; },
    get() { return 2; },
    getOwnPropertyDescriptor() { return { value: 1, writable: true }; },
  });
  return typeError(() => Reflect.ownKeys(a)) && typeError(() => a.length) &&
    typeError(() => Object.getOwnPropertyDescriptor(a, "0"));
}

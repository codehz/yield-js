// @pending ["wait:integrity"]
// @restore
// @fulfill ["wait:integrity", null]
// @fulfilled true

export async function run() {
  const sym = Symbol("state");
  let state = 0;
  function C() {}
  C.prototype.inherited = 1;
  Object.defineProperty(C.prototype, "sink", {set(v) { state = v; }});
  const fixed = new C();
  fixed.live = 1;
  fixed.removable = 2;
  if (!Reflect.isExtensible(fixed) || !Reflect.preventExtensions(fixed)) return false;

  const sealed = [1, 2, 3];
  delete sealed[1];
  if (Object.seal(sealed) !== sealed) return false;
  const frozen = [4];
  Object.defineProperty(frozen, "hidden", {value: {n: 1}, writable: true, configurable: true});
  frozen[sym] = 5;
  Object.defineProperty(frozen, "access", {
    configurable: true,
    get() { state++; return state; },
    set(v) { state = v; }
  });
  if (Object.freeze(frozen) !== frozen || state !== 0) return false;

  const savedIsFrozen = Object.isFrozen;
  const promise = Object.freeze(Promise.resolve(9));
  const fn = Object.freeze(function () { return 10; });
  const native = bind("integrity");
  await wait("integrity");

  if (Object.isExtensible(fixed) || Object.isSealed(fixed) || Object.isFrozen(fixed)) return false;
  if (Object.preventExtensions(fixed) !== fixed || !Reflect.preventExtensions(fixed)) return false;
  fixed.live = 3;
  fixed.sink = 7;
  if (fixed.live !== 3 || state !== 7 || !delete fixed.removable) return false;
  try { fixed.inherited = 2; return false; } catch (e) { if (e.name !== "TypeError") return false; }
  if (fixed.inherited !== 1 || fixed.hasOwnProperty("inherited")) return false;
  if (Reflect.defineProperty(fixed, "removable", {}) || Reflect.defineProperty(fixed, sym, {})) return false;
  try {
    Object.defineProperties(fixed, {live: {value: 8}, absent: {value: 4}});
    return false;
  } catch (e) { if (e.name !== "TypeError" || fixed.live !== 8) return false; }

  if (!Object.isSealed(sealed) || savedIsFrozen(sealed) || Reflect.deleteProperty(sealed, 2)) return false;
  sealed[0] = 8;
  sealed.length = 6; // Length may grow without creating any index properties.
  if (sealed.length !== 6 || sealed[0] !== 8 || 5 in sealed) return false;
  if (Reflect.defineProperty(sealed, 1, {}) || Reflect.defineProperty(sealed, 7, {})) return false;
  try { sealed[1] = 3; return false; } catch (e) { if (e.name !== "TypeError") return false; }
  if (Reflect.defineProperty(sealed, "length", {value: 1, writable: false})) return false;
  if (sealed.length !== 3 || Object.getOwnPropertyDescriptor(sealed, "length").writable) return false;
  if (savedIsFrozen(sealed)) return false; // Existing elements are still writable.

  if (!savedIsFrozen(frozen) || !Object.isSealed(frozen) || Reflect.isExtensible(frozen)) return false;
  if (Reflect.defineProperty(frozen, sym, {value: 6}) || !Reflect.defineProperty(frozen, sym, {value: 5})) return false;
  if (Reflect.defineProperty(frozen, "access", {set: undefined})) return false;
  if (Reflect.deleteProperty(frozen, "access") || Object.getOwnPropertyDescriptor(frozen, "hidden").writable) return false;
  try { frozen.length = 1; return false; } catch (e) { if (e.name !== "TypeError") return false; }
  frozen.access = 11;
  frozen.hidden.n = 2;
  if (state !== 11 || frozen.access !== 12 || frozen.hidden.n !== 2) return false;
  if (!savedIsFrozen(fn) || fn() !== 10 || !savedIsFrozen(promise) || (await promise) !== 9) return false;
  if (Object.isExtensible(native) || !savedIsFrozen(native) || Reflect.defineProperty(native, "x", {})) return false;

  if (Object.preventExtensions(7) !== 7 || Object.seal(null) !== null || Object.freeze() !== undefined) return false;
  if (Object.isExtensible("x") || !Object.isSealed(null) || !savedIsFrozen(undefined)) return false;
  try { Reflect.preventExtensions(7); return false; } catch (e) { if (e.name !== "TypeError") return false; }
  try { Reflect.isExtensible(null); return false; } catch (e) { if (e.name !== "TypeError") return false; }
  try { Reflect.defineProperty(frozen, "length", {value: -1}); return false; } catch (e) { return e.name === "RangeError"; }
}

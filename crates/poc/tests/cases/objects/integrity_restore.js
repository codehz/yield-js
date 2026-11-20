// @pending ["wait:integrity"]
// @restore
// @fulfill ["wait:integrity", null]
// @fulfilled undefined

import { expect, bind, wait } from 'test';

export async function run() {
  const sym = Symbol("state");
  let state = 0;
  function C() {}
  C.prototype.inherited = 1;
  Object.defineProperty(C.prototype, "sink", {set(v) { state = v; }});
  const fixed = new C();
  fixed.live = 1;
  fixed.removable = 2;
  expect(Reflect.isExtensible(fixed)).toBe(true);
  expect(Reflect.preventExtensions(fixed)).toBe(true);

  const sealed = [1, 2, 3];
  delete sealed[1];
  expect(Object.seal(sealed)).toBe(sealed);
  const frozen = [4];
  Object.defineProperty(frozen, "hidden", {value: {n: 1}, writable: true, configurable: true});
  frozen[sym] = 5;
  Object.defineProperty(frozen, "access", {
    configurable: true,
    get() { state++; return state; },
    set(v) { state = v; }
  });
  expect(Object.freeze(frozen)).toBe(frozen);
  expect(state).toBe(0);

  const savedIsFrozen = Object.isFrozen;
  const promise = Object.freeze(Promise.resolve(9));
  const fn = Object.freeze(function () { return 10; });
  const native = bind("integrity");
  Object.freeze(native);
  await wait("integrity");

  expect(Object.isExtensible(fixed)).toBe(false);
  expect(Object.isSealed(fixed)).toBe(false);
  expect(Object.isFrozen(fixed)).toBe(false);
  expect(Object.preventExtensions(fixed)).toBe(fixed);
  expect(Reflect.preventExtensions(fixed)).toBe(true);
  fixed.live = 3;
  fixed.sink = 7;
  expect(fixed.live).toBe(3);
  expect(state).toBe(7);
  expect(delete fixed.removable).toBe(true);
  let inheritedWriteError;
  try {
    fixed.inherited = 2;
  } catch (caught) {
    inheritedWriteError = caught;
  }
  expect(inheritedWriteError?.name).toBe("TypeError");
  expect(fixed.inherited).toBe(1);
  expect(fixed.hasOwnProperty("inherited")).toBe(false);
  expect(Reflect.defineProperty(fixed, "removable", {})).toBe(false);
  expect(Reflect.defineProperty(fixed, sym, {})).toBe(false);
  let definePropertiesError;
  try {
    Object.defineProperties(fixed, {live: {value: 8}, absent: {value: 4}});
  } catch (caught) {
    definePropertiesError = caught;
  }
  expect(definePropertiesError?.name).toBe("TypeError");
  expect(fixed.live).toBe(8);

  expect(Object.isSealed(sealed)).toBe(true);
  expect(savedIsFrozen(sealed)).toBe(false);
  expect(Reflect.deleteProperty(sealed, 2)).toBe(false);
  sealed[0] = 8;
  sealed.length = 6; // Length may grow without creating any index properties.
  expect(sealed.length).toBe(6);
  expect(sealed[0]).toBe(8);
  expect(5 in sealed).toBe(false);
  expect(Reflect.defineProperty(sealed, 1, {})).toBe(false);
  expect(Reflect.defineProperty(sealed, 7, {})).toBe(false);
  let sealedWriteError;
  try {
    sealed[1] = 3;
  } catch (caught) {
    sealedWriteError = caught;
  }
  expect(sealedWriteError?.name).toBe("TypeError");
  expect(Reflect.defineProperty(sealed, "length", {value: 1, writable: false})).toBe(false);
  expect(sealed.length).toBe(3);
  expect(Object.getOwnPropertyDescriptor(sealed, "length").writable).toBe(false);
  expect(savedIsFrozen(sealed)).toBe(false); // Existing elements are still writable.

  expect(savedIsFrozen(frozen)).toBe(true);
  expect(Object.isSealed(frozen)).toBe(true);
  expect(Reflect.isExtensible(frozen)).toBe(false);
  expect(Reflect.defineProperty(frozen, sym, {value: 6})).toBe(false);
  expect(Reflect.defineProperty(frozen, sym, {value: 5})).toBe(true);
  expect(Reflect.defineProperty(frozen, "access", {set: undefined})).toBe(false);
  expect(Reflect.deleteProperty(frozen, "access")).toBe(false);
  expect(Object.getOwnPropertyDescriptor(frozen, "hidden").writable).toBe(false);
  let frozenLengthError;
  try {
    frozen.length = 1;
  } catch (caught) {
    frozenLengthError = caught;
  }
  expect(frozenLengthError?.name).toBe("TypeError");
  frozen.access = 11;
  frozen.hidden.n = 2;
  expect(state).toBe(11);
  expect(frozen.access).toBe(12);
  expect(frozen.hidden.n).toBe(2);
  expect(savedIsFrozen(fn)).toBe(true);
  expect(fn()).toBe(10);
  expect(savedIsFrozen(promise)).toBe(true);
  expect((await promise)).toBe(9);
  expect(Object.isExtensible(native)).toBe(false);
  expect(savedIsFrozen(native)).toBe(true);
  expect(Reflect.defineProperty(native, "x", {})).toBe(false);

  expect(Object.preventExtensions(7)).toBe(7);
  expect(Object.seal(null)).toBe(null);
  expect(Object.freeze()).toBe(undefined);
  expect(Object.isExtensible("x")).toBe(false);
  expect(Object.isSealed(null)).toBe(true);
  expect(savedIsFrozen(undefined)).toBe(true);
  let preventExtensionsError;
  try {
    Reflect.preventExtensions(7);
  } catch (caught) {
    preventExtensionsError = caught;
  }
  expect(preventExtensionsError?.name).toBe("TypeError");
  let isExtensibleError;
  try {
    Reflect.isExtensible(null);
  } catch (caught) {
    isExtensibleError = caught;
  }
  expect(isExtensibleError?.name).toBe("TypeError");
  let invalidLengthError;
  try {
    Reflect.defineProperty(frozen, "length", {value: -1});
  } catch (caught) {
    invalidLengthError = caught;
  }
  expect(invalidLengthError?.name).toBe("RangeError");
}

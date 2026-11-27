// @returns true

import { assert } from "test";

function throws(fn) {
  try {
    fn();
    return false;
  } catch (e) {
    return e instanceof TypeError;
  }
}

class DerivedRef extends WeakRef {}
class DerivedReg extends FinalizationRegistry {}

export function run() {
  const object = {};
  const fn = function target() {};
  const local = Symbol("local");
  const refObject = new WeakRef(object);
  const refFn = new WeakRef(fn);
  const refSym = new WeakRef(local);
  assert(refObject.deref() === object);
  assert(refFn.deref() === fn);
  assert(refSym.deref() === local);

  assert(throws(() => new WeakRef(Symbol.for("registered"))));
  assert(throws(() => new WeakRef(1)));
  assert(throws(() => new WeakRef("x")));
  assert(throws(() => WeakRef(object)));
  assert(throws(() => WeakRef.prototype.deref.call({})));
  assert(throws(() => WeakRef.prototype.deref.call(undefined)));

  assert(throws(() => new FinalizationRegistry(1)));
  assert(throws(() => new FinalizationRegistry({})));
  assert(throws(() => FinalizationRegistry(() => {})));
  const registry = new FinalizationRegistry(() => {});
  assert(throws(() => registry.register(1, "held")));
  assert(throws(() => registry.register(object, object)));
  assert(throws(() => registry.register(object, "held", 1)));
  assert(throws(() => registry.register(object, "held", Symbol.for("tok"))));
  assert(throws(() => FinalizationRegistry.prototype.register.call({}, object, "h")));
  assert(throws(() => registry.unregister(1)));
  assert(throws(() => FinalizationRegistry.prototype.unregister.call({}, object)));

  const deref = Object.getOwnPropertyDescriptor(WeakRef.prototype, "deref");
  assert(deref.enumerable === false && deref.writable === true && deref.configurable === true);
  assert(WeakRef.prototype.deref.length === 0);
  assert(WeakRef.length === 1);
  const tag = Object.getOwnPropertyDescriptor(WeakRef.prototype, Symbol.toStringTag);
  assert(tag.value === "WeakRef" && !tag.enumerable && !tag.writable && tag.configurable);
  const register = Object.getOwnPropertyDescriptor(FinalizationRegistry.prototype, "register");
  assert(register.enumerable === false);
  assert(FinalizationRegistry.prototype.register.length === 2);
  assert(FinalizationRegistry.prototype.unregister.length === 1);
  assert(FinalizationRegistry.length === 1);
  const rtag = Object.getOwnPropertyDescriptor(FinalizationRegistry.prototype, Symbol.toStringTag);
  assert(rtag.value === "FinalizationRegistry" && !rtag.enumerable && !rtag.writable && rtag.configurable);

  const derivedTarget = {};
  const derived = new DerivedRef(derivedTarget);
  assert(Object.getPrototypeOf(derived) === DerivedRef.prototype);
  assert(derived.deref() === derivedTarget);
  const derivedReg = new DerivedReg(() => {});
  assert(Object.getPrototypeOf(derivedReg) === DerivedReg.prototype);

  const token = {};
  const a = {};
  const b = {};
  registry.register(a, "a", token);
  registry.register(b, "b", token);
  assert(registry.unregister(token) === true);
  assert(registry.unregister(token) === false);

  return true;
}

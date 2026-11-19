// @fulfilled 3

export async function run() {
  if (typeof globalThis !== "object" || globalThis.globalThis !== globalThis ||
      Object.getPrototypeOf(globalThis) !== Object.prototype) return "identity or type";

  const globals = {
    Promise, queueMicrotask, Error, TypeError, Symbol, Reflect, Object, Array, String,
    RangeError, ReferenceError, SyntaxError, EvalError, URIError, AggregateError,
    Number, Math, JSON, Map, Set, WeakMap, Proxy, Boolean, Infinity, undefined, globalThis
  };
  for (const name of Object.keys(globals)) {
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, name);
    if (!descriptor || !descriptor.writable || !descriptor.configurable ||
        !descriptor.enumerable || descriptor.value !== globals[name]) return name;
  }
  const nan = Object.getOwnPropertyDescriptor(globalThis, "NaN");
  if (!nan || !nan.writable || !nan.configurable || !nan.enumerable ||
      !Object.is(nan.value, NaN)) return "NaN";
  if (globalThis.Missing !== undefined || typeof globalThis.MessageChannel !== "undefined" ||
      "Function" in globalThis || "wait" in globalThis) return "unexpected global";

  globalThis.x = 1;
  if (globalThis.x !== 1) return "assignment";
  try {
    globalThis();
    return "called globalThis";
  } catch (error) {
    if (!(error instanceof TypeError) || error.message !== "value is not a function") {
      return "wrong call error";
    }
  }
  return await globalThis.Promise.resolve(3);
}

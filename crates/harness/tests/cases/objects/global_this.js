// @fulfilled 3

export async function run() {
  if (typeof globalThis !== "object" || globalThis.globalThis !== globalThis ||
      Object.getPrototypeOf(globalThis) !== Object.prototype) return "identity or type";

  const globals = {
    Promise, queueMicrotask, Error, TypeError, Symbol, Reflect, Object, Array, String,
    RangeError, ReferenceError, SyntaxError, EvalError, URIError, AggregateError,
    Number, Math, JSON, Map, Set, WeakMap, Proxy, Boolean, Infinity, undefined, globalThis, Function
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
      "wait" in globalThis) return "unexpected global";

  const proto = Function.prototype;
  if (proto() !== undefined || proto.constructor !== Function || !(run instanceof Function)) return 'Function prototype';
  if (Object.getPrototypeOf(proto) !== Object.prototype || Object.getPrototypeOf(Function) !== proto) return 'Function inheritance';
  const caller = Object.getOwnPropertyDescriptor(proto, 'caller');
  const args = Object.getOwnPropertyDescriptor(proto, 'arguments');
  if (caller.get !== caller.set || caller.get !== args.get || args.get !== args.set ||
      Object.isExtensible(caller.get) || !caller.configurable || caller.enumerable) return 'restricted descriptors';
  let conversions = 0;
  const body = {toString() { ++conversions; return 'return 1'; }};
  const newTarget = Object.bind(null);
  Object.defineProperty(newTarget, 'prototype', {get() { throw 'newTarget.prototype was read'; }});
  for (const attempt of [() => Function(body), () => new Function(body), () => run.constructor(body),
                         () => Reflect.construct(Function, [body], newTarget)]) {
    try { attempt(); return 'dynamic construction succeeded'; } catch (error) {
      if (!(error instanceof TypeError) || error.message !== 'Dynamic function construction is disabled') return 'dynamic construction error';
    }
  }
  if (conversions !== 0) return 'parameter coercion';
  for (const attempt of [() => new proto(), () => run.caller, () => { run.arguments = 0; }]) {
    try { attempt(); return 'restricted operation succeeded'; } catch (error) {
      if (!(error instanceof TypeError)) return 'restricted operation error';
    }
  }
  if (!delete proto.call || run.call !== undefined || 'call' in run) return 'virtual call';
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

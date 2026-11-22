// @returns undefined

import { expect, gc } from "test";

function throwsTypeError(operation) {
  try {
    operation();
  } catch (error) {
    expect(error instanceof TypeError).toBe(true);
    return;
  }
  throw "expected TypeError";
}

export function run() {
  let calls = 0;
  const pair = Proxy.revocable([42], { get(t, k) { calls++; return t[k]; } });
  const { proxy, revoke } = pair;
  const outer = new Proxy(proxy, {});
  const child = Object.create(proxy);
  expect(Proxy.revocable.length).toBe(2);
  expect(Object.keys(pair).join(",")).toBe("proxy,revoke");
  expect(revoke.name).toBe("");
  expect(revoke.length).toBe(0);
  expect(Object.getPrototypeOf(revoke)).toBe(Function.prototype);
  throwsTypeError(() => new revoke());
  throwsTypeError(() => Proxy.revocable(null, {}));
  throwsTypeError(() => Proxy.revocable({}, null));
  expect(Array.isArray(outer)).toBe(true);
  expect(proxy[0]).toBe(42);
  expect(calls).toBe(1);

  const other = Proxy.revocable({}, {});
  expect(revoke.call(other.proxy, other.revoke)).toBe(undefined);
  expect(revoke()).toBe(undefined);
  expect(Reflect.isExtensible(other.proxy)).toBe(true);
  expect(typeof proxy).toBe("object");
  expect(proxy === pair.proxy).toBe(true);
  for (const operation of [
    () => proxy[0],
    () => { proxy[0] = 5; },
    () => 0 in proxy,
    () => delete proxy[0],
    () => Reflect.get(child, "missing"),
    () => Reflect.set(child, "missing", 1),
    () => Object.keys(proxy),
    () => Object.getOwnPropertyDescriptor(proxy, "0"),
    () => Reflect.defineProperty(proxy, "x", { value: 1 }),
    () => Reflect.getPrototypeOf(proxy),
    () => Reflect.setPrototypeOf(proxy, null),
    () => Reflect.isExtensible(proxy),
    () => Reflect.preventExtensions(proxy),
    () => Object.isFrozen(proxy),
    () => Object.freeze(proxy),
    () => Object.prototype.toString.call(proxy),
    () => proxy instanceof Array,
    () => Array.prototype.isPrototypeOf(child),
    () => Array.isArray(proxy),
    () => Array.isArray(outer),
    () => Array.isArray(new Proxy(proxy, {})),
    () => [proxy].flat(),
  ]) throwsTypeError(operation);
  expect(calls).toBe(1);

  // Revoking during trap lookup does not invalidate the slots captured by
  // the current operation, even if the getter triggers a collection.
  const duringLookup = Proxy.revocable({ answer: 42 }, {
    get ownKeys() {
      duringLookup.revoke();
      gc();
      return undefined;
    },
  });
  expect(Reflect.ownKeys(duringLookup.proxy).join(",")).toBe("answer");
  throwsTypeError(() => Reflect.ownKeys(duringLookup.proxy));
}

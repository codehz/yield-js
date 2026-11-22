// @returns true

export function run() {
  const marker = Symbol("marker");
  const target = {
    get method() {
      const receiver = this;
      return function () { return this === receiver && this[marker] === 7; };
    },
    [marker]: 7,
  };
  let called = false;
  const proxy = new Proxy(target, {
    get(object, key, receiver) {
      if (key === "method") called = receiver === proxy;
      return Reflect.get(object, key, receiver);
    },
  });
  const missing = null;
  const typed = new Uint8Array([23]);
  let caught = false;
  try {
    missing.method;
  } catch (error) {
    caught = error instanceof TypeError;
  }
  return proxy?.["method"]() && called && caught &&
    missing?.method === undefined && typed["0"] === 23 &&
    typed["-0"] === undefined && "abc"[`1`] === "b";
}

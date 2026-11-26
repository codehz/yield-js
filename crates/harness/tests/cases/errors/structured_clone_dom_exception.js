// @pending ["wait:dom-exception"]
// @restore
// @fulfill ["wait:dom-exception", null]
// @fulfilled true

import { assert, wait } from "test";

export async function run() {
  const empty = new DOMException(undefined, undefined);
  assert(empty.name === "Error" && empty.message === "" && empty.code === 0);
  assert(new DOMException(null, "ValidationError").message === "null");
  assert(new DOMException("", "ValidationError").code === 0);
  assert(Object.getPrototypeOf(DOMException) === Error);
  assert(Object.getPrototypeOf(DOMException.prototype) === Error.prototype);
  assert(DOMException.length === 0);

  let order = "";
  class Child extends DOMException {}
  const error = new Child(
    { toString() { order += "m"; return "message\ud800"; } },
    { toString() { order += "n"; return "DataCloneError"; } },
  );
  assert(order === "mn");
  assert(error instanceof Child && error instanceof DOMException && error instanceof Error);
  assert(Error.isError(error));
  assert(Object.prototype.toString.call(error) === "[object DOMException]");
  assert(error.toString() === "DataCloneError: message\ud800");

  try { DOMException(); assert(false); } catch (thrown) { assert(thrown instanceof TypeError); }
  try { new DOMException(Symbol()); assert(false); } catch (thrown) { assert(thrown instanceof TypeError); }
  const name = Object.getOwnPropertyDescriptor(DOMException.prototype, "name");
  assert(name.enumerable && name.configurable && name.set === undefined && name.get.name === "get name");
  const fake = Object.create(DOMException.prototype);
  assert(!Error.isError(fake) && !Error.isError(DOMException.prototype));
  try { name.get.call(fake); assert(false); } catch (thrown) { assert(thrown instanceof TypeError); }
  try { name.get.call(new Error()); assert(false); } catch (thrown) { assert(thrown instanceof TypeError); }
  const proxy = new Proxy(error, {});
  assert(!Error.isError(proxy));
  try { name.get.call(proxy); assert(false); } catch (thrown) { assert(thrown instanceof TypeError); }
  const constant = Object.getOwnPropertyDescriptor(DOMException, "DATA_CLONE_ERR");
  assert(constant.value === 25 && constant.enumerable && !constant.writable && !constant.configurable);
  assert(DOMException.prototype.DATA_CLONE_ERR === 25 && DOMException.VALIDATION_ERR === 16);

  Object.defineProperty(error, "name", { value: "shadowed" });
  const detached = new DOMException("detached", "AbortError");
  Object.setPrototypeOf(detached, null);
  await wait("dom-exception");

  assert(error instanceof Child && error instanceof DOMException && error instanceof Error);
  assert(Error.isError(error) && Error.isError(detached));
  assert(error.name === "shadowed" && name.get.call(error) === "DataCloneError");
  assert(error.message === "message\ud800" && error.code === DOMException.DATA_CLONE_ERR);
  assert(Object.getPrototypeOf(detached) === null && name.get.call(detached) === "AbortError");
  return true;
}

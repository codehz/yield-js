// @pending ["wait:error-brand"]
// @restore
// @fulfill ["wait:error-brand", null]
// @fulfilled true

import { assert, wait } from "test";

export async function run() {
  class Child extends Error {}
  const branded = new Child("message");
  Object.setPrototypeOf(branded, null);
  const isError = Error.isError;
  await wait("error-brand");
  assert(isError(branded));
  assert(isError(new AggregateError([])));
  assert(!isError(Object.create(Error.prototype)));
  assert(!isError({ [Symbol.toStringTag]: "Error" }));
  assert(!isError());
  try { null.missing; } catch (error) { return isError(error); }
  return false;
}

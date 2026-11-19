// @returns true

function throwsTypeError(value, constructor) {
  try {
    value instanceof constructor;
    return false;
  } catch (e) {
    return e instanceof TypeError;
  }
}

export function run() {
  function C() {}
  C.prototype = 1;
  const arrow = () => {};
  const method = { m() {} }.m;
  const asyncFunction = async () => {};
  return throwsTypeError({}, {}) && throwsTypeError(0, null) &&
    throwsTypeError({}, C) && throwsTypeError({}, arrow) &&
    throwsTypeError({}, method) && throwsTypeError({}, asyncFunction) &&
    throwsTypeError({}, arrow.bind(null));
}

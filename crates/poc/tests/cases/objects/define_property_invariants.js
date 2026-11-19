// @fulfilled true

function rejects(action) {
  try { action(); } catch (error) { return error.name === "TypeError"; }
  return false;
}

async function run() {
  const define = Object.defineProperty;
  const target = {};
  const read = function () { return 12; };
  define(target, "zero", { value: -0 });
  define(target, "nan", { value: 0 / 0 });
  define(target, "value", { value: 1, writable: true });
  define(target, "read", { get: read });
  define(target, "zero", { value: -0 });
  define(target, "nan", { value: 0 / 0 });
  define(target, "value", { value: 2, writable: false });
  define(target, "read", { get: read, set: undefined });
  if (!rejects(() => define(target, "zero", { value: 0 })) ||
      !rejects(() => define(target, "value", { writable: true })) ||
      !rejects(() => define(target, "value", { enumerable: true })) ||
      !rejects(() => define(target, "value", { configurable: true })) ||
      !rejects(() => define(target, "value", { get: read })) ||
      !rejects(() => define(target, "read", { get: undefined })) ||
      !rejects(() => define(target, "read", { value: undefined })) ||
      !rejects(() => define(target, "bad", { get: read, value: undefined })) ||
      !rejects(() => define(target, "bad", { set: null })) ||
      !rejects(() => define(target, "bad", null)) ||
      !rejects(() => define(1, "bad", {}))) return false;
  try {
    define(target, "bad", { get value() { throw "descriptor"; } });
    return false;
  } catch (error) {
    if (error !== "descriptor") return false;
  }
  return target.value === 2 && target.read === 12 && !("bad" in target);
}

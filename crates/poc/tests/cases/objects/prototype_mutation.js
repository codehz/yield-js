// @fulfilled true

function throwsTypeError(action) {
  try { action(); return false; } catch (e) { return e.name === "TypeError"; }
}

export async function run() {
  const a = Object.create(null, { value: { value: 1 } });
  const b = Object.create(a);
  const c = Object.create(b);
  if (Reflect.setPrototypeOf(a, a) !== false || Reflect.setPrototypeOf(a, c) !== false) return false;
  if (!throwsTypeError(() => Object.setPrototypeOf(b, c))) return false;
  if (Object.getPrototypeOf(a) !== null || Object.getPrototypeOf(b) !== a || c.value !== 1) return false;

  const replacement = { next: 2 };
  if (Object.setPrototypeOf(b, replacement) !== b || Reflect.getPrototypeOf(b) !== replacement) return false;
  if ("value" in c || !("next" in c) || c.next !== 2) return false;
  if (!Reflect.setPrototypeOf(b, null) || Object.getPrototypeOf(b) !== null || "next" in c) return false;

  const fixed = Object.freeze(c);
  if (!Reflect.setPrototypeOf(fixed, b) || Object.setPrototypeOf(fixed, b) !== fixed) return false;
  if (Reflect.setPrototypeOf(fixed, null) !== false) return false;
  if (!throwsTypeError(() => Object.setPrototypeOf(fixed, a)) || Object.getPrototypeOf(fixed) !== b) return false;

  const root = Object.prototype;
  if (!Object.isExtensible(root) || Reflect.getPrototypeOf(root) !== null) return false;
  if (!Reflect.setPrototypeOf(root, null) || Object.setPrototypeOf(root, null) !== root) return false;
  // A null-rooted candidate cannot introduce a cycle; the root link itself is immutable.
  if (Reflect.setPrototypeOf(root, a) !== false || !throwsTypeError(() => Object.setPrototypeOf(root, a))) return false;
  return Reflect.getPrototypeOf(root) === null;
}

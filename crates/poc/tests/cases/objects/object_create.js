// @fulfilled true

export async function run() {
  const key = Symbol("inherited");
  const proto = Object.create(null, {
    [key]: { value: 8 },
    count: { value: 2, writable: true },
    score: {
      get() { return this.count + 1; },
      set(value) { this.count = value - 1; },
    },
  });
  if (Object.getPrototypeOf(proto) !== null || "toString" in proto) return false;

  const descriptors = Object.create({ ignored: { value: 10 } });
  descriptors.hidden = { value: 4 };
  const child = Object.create(proto, descriptors);
  const hidden = Object.getOwnPropertyDescriptor(child, "hidden");
  if (hidden.value !== 4 || hidden.enumerable || hidden.configurable || hidden.writable) return false;
  if (Reflect.getPrototypeOf(child) !== proto || "ignored" in child || "toString" in child) return false;
  if (!(key in child) || child[key] !== 8 || !("score" in child) || child.score !== 3) return false;
  if (Object.getOwnPropertyDescriptor(child, key) !== undefined) return false;

  child.score = 12;
  if (child.score !== 12 || child.count !== 11 || proto.count !== 2) return false;
  if (Object.getOwnPropertyDescriptor(child, "score") !== undefined) return false;
  try { child[key] = 9; return false; } catch (e) { if (e.name !== "TypeError") return false; }
  if (child[key] !== 8 || Object.getOwnPropertyDescriptor(child, key) !== undefined) return false;

  const marker = {};
  try {
    Object.create(null, { get bad() { throw marker; } });
    return false;
  } catch (e) {
    return e === marker && Object.getPrototypeOf(Object.create(null, undefined)) === null;
  }
}

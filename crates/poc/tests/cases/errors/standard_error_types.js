// @returns true
export function run() {
  const types = [
    [RangeError, "RangeError"], [ReferenceError, "ReferenceError"],
    [SyntaxError, "SyntaxError"], [EvalError, "EvalError"],
    [URIError, "URIError"],
  ];
  for (const [Ctor, name] of types) {
    const called = Ctor("custom");
    const made = new Ctor();
    if (!(called instanceof Ctor) || !(made instanceof Ctor)) return false;
    if (called.name !== name || made.name !== name) return false;
    if (called.message !== "custom" || made.message !== "") return false;
    if (Object.getPrototypeOf(Ctor.prototype) !== Error.prototype || Ctor.prototype.constructor !== Ctor) return false;
  }
  const list = new AggregateError([1, 2], "array");
  const string = AggregateError("ab", "string");
  const iterable = AggregateError({
    [Symbol.iterator]() {
      let i = 0;
      return { next() { return i < 2 ? {value: ++i, done: false} : {done: true}; } };
    }
  });
  if (!(list instanceof AggregateError) || list.name !== "AggregateError" || list.message !== "array") return false;
  if (list.errors.length !== 2 || list.errors[0] !== 1 || list.errors[1] !== 2) return false;
  if (string.errors.length !== 2 || string.errors[0] !== "a" || string.errors[1] !== "b") return false;
  if (iterable.errors.length !== 2 || iterable.errors[0] !== 1 || iterable.errors[1] !== 2) return false;
  if (AggregateError().errors.length !== 0 || AggregateError(undefined).errors.length !== 0) return false;
  try { AggregateError(null); return false; } catch (error) { if (!(error instanceof TypeError)) return false; }
  return true;
}

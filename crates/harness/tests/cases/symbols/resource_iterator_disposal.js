// @fulfilled true

export async function run() {
  function* gen() { yield 1; }
  async function* agen() { yield 1; }
  const iterators = [[1].values(), "x"[Symbol.iterator](), new Map().keys(),
    new Set().values(), "x".matchAll(/x/g), gen()];
  const dispose = iterators[0][Symbol.dispose];
  for (const it of iterators) {
    if (it[Symbol.dispose] !== dispose || it.hasOwnProperty(Symbol.dispose)) return false;
    let called = false;
    it.return = function (...args) {
      called = this === it && args.length === 0;
      return 42;
    };
    if (it[Symbol.dispose]() !== undefined || !called) return false;
  }
  delete iterators[0].return;
  if (iterators[0][Symbol.dispose]() !== undefined) return false;

  const it = agen();
  let called = false;
  it.return = function (...args) {
    called = this === it && args.length === 1 && args[0] === undefined;
    return { then(resolve) { resolve(42); } };
  };
  if (await it[Symbol.asyncDispose]() !== undefined || !called) return false;
  Object.defineProperty(it, "return", { get() { throw "getter"; } });
  const rejected = it[Symbol.asyncDispose]();
  try { await rejected; } catch (error) { return error === "getter"; }
  return false;
}

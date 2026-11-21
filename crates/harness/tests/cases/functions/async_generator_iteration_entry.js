// @fulfilled "undefined|true|function|false"

export async function run() {
  async function* g() {
    yield 1;
  }
  const it = g();
  const syncEntry = it[Symbol.iterator];
  // Only Symbol.asyncIterator exists, and it returns the receiver itself.
  const self = it[Symbol.asyncIterator]() === it;
  const entryType = typeof it[Symbol.asyncIterator];
  // The async generator prototype never inherits the synchronous one.
  const inherits =
    Object.getPrototypeOf(Object.getPrototypeOf(it)) ===
    Object.getPrototypeOf(Object.getPrototypeOf((function* () {})()));
  return `${typeof syncEntry}|${self}|${entryType}|${inherits}`;
}

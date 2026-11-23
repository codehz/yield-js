// @advance 0
// @fulfilled true

export function run() {
  for (const input of [undefined, NaN, Infinity, -1, 2 ** 53, 1n, Symbol()]) {
    try { AbortSignal.timeout(input); return Promise.resolve(false); }
    catch (error) { if (!(error instanceof TypeError)) return Promise.resolve(false); }
  }
  const wide = AbortSignal.timeout(2 ** 32);
  const zero = AbortSignal.timeout({ valueOf() { return -0.9; } });
  if (wide.aborted || zero.aborted) return Promise.resolve(false);
  return new Promise(resolve => { zero.onabort = () => resolve(!wide.aborted && zero.reason.code === 23); });
}

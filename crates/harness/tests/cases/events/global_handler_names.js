// @returns true

export function run() {
  if (typeof onerror !== 'object' || onerror !== null) return false;
  const f = function () { return this; };
  onerror ??= f;
  if (globalThis.onerror !== f || typeof onerror !== 'function' || onerror() !== undefined) return false;
  { let onerror = 2; onerror++; if (onerror !== 3) return false; }
  if (onerror !== f) return false;
  ({ f: onunhandledrejection } = { f });
  [onrejectionhandled] = [f];
  if (globalThis.onunhandledrejection !== f || globalThis.onrejectionhandled !== f) return false;
  onerror = null;
  if (++onerror !== 1 || onerror !== null) return false;
  onunhandledrejection = null;
  if ((onunhandledrejection += 2) !== 2 || onunhandledrejection !== null) return false;
  globalThis.notAKnownHandler = 21;
  return typeof notAKnownHandler === 'undefined';
}

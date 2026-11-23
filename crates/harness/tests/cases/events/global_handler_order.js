// @returns true

export function run() {
  const g = globalThis;
  const desc = Object.getOwnPropertyDescriptor(g, 'onerror');
  if (g.onerror !== null || g.onunhandledrejection !== null || g.onrejectionhandled !== null ||
      !desc.enumerable || !desc.configurable || typeof desc.get !== 'function' ||
      (new EventTarget()).onerror !== undefined) return false;
  const log = [];
  g.addEventListener('error', () => { log.push('first'); return false; });
  const original = () => { log.push('original'); };
  g.onerror = original;
  g.addEventListener('error', () => { log.push('last'); });
  function handler(e) {
    log.push(this === g && e instanceof Event && arguments.length === 1 ? 'handler' : 'wrong');
    return false;
  }
  g.onerror = handler;
  g.addEventListener('error', handler);
  g.removeEventListener('error', handler);
  if (g.dispatchEvent(new Event('error', { cancelable: true }))) return false;
  g.onerror = 0;
  if (g.onerror !== null) return false;
  g.onerror = handler;
  if (g.dispatchEvent(new Event('error', { cancelable: true }))) return false;
  const obj = { handleEvent() { log.push('unexpected'); } };
  g.onerror = obj;
  if (g.onerror !== obj || !g.dispatchEvent(new Event('error', { cancelable: true }))) return false;
  return log.join(',') === 'first,handler,last,first,last,handler,first,last';
}

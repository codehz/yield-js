// @advance 1
// @uncaught ["listener", "microtask", "timer"]

export function run() {
  globalThis.addEventListener('error', e => {
    if (!e.isTrusted || e.message !== e.error) throw 'bad error event';
    if (e.error === 'hidden') e.preventDefault();
  });
  const target = new EventTarget();
  target.addEventListener('x', () => { throw 'listener'; });
  target.addEventListener('x', () => { throw 'hidden'; });
  target.dispatchEvent(new Event('x'));
  queueMicrotask(() => { throw 'microtask'; });
  queueMicrotask(() => { throw 'hidden'; });
  setTimeout(() => { throw 'timer'; }, 1);
  setTimeout(() => { throw 'hidden'; }, 1);
}

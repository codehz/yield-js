// @returns true

export function run() {
  const order = [];
  const marker = {};
  const init = {
    get bubbles() { order.push('bubbles'); return true; },
    get cancelable() { order.push('cancelable'); return true; },
    get composed() { order.push('composed'); return true; },
    get colno() { order.push('colno'); return -1; },
    get error() { order.push('error'); return marker; },
    get filename() { order.push('filename'); return '\ud800'; },
    get lineno() { order.push('lineno'); return 4294967297; },
    get message() { order.push('message'); return '\ud800'; },
  };
  class Derived extends ErrorEvent {}
  const e = new Derived('error', init);
  if (!(e instanceof Derived && e instanceof Event) || e.isTrusted || !e.bubbles ||
      !e.cancelable || !e.composed || e.type !== 'error' || e.error !== marker ||
      e.filename !== '\ufffd' || e.message !== '\ud800' || e.lineno !== 1 ||
      e.colno !== 4294967295 || Object.hasOwn(e, 'error') ||
      order.join(',') !== 'bubbles,cancelable,composed,colno,error,filename,lineno,message') return false;
  const d = new ErrorEvent('x');
  if (d.message !== '' || d.filename !== '' || d.lineno !== 0 || d.colno !== 0 || d.error !== undefined) return false;
  const p = new PromiseRejectionEvent('x', { promise: marker, reason: e });
  if (!(p instanceof Event) || p.isTrusted || p.promise !== marker || p.reason !== e ||
      new PromiseRejectionEvent('x', { promise: marker }).reason !== undefined) return false;
  try { new ErrorEvent('x', { get message() { throw marker; } }); return false; }
  catch (e) { if (e !== marker) return false; }
  for (const bad of [() => ErrorEvent('x'), () => new ErrorEvent(),
                     () => PromiseRejectionEvent('x', { promise: marker }),
                     () => new PromiseRejectionEvent('x'),
                     () => new PromiseRejectionEvent('x', { promise: 1 }),
                     () => Object.getOwnPropertyDescriptor(ErrorEvent.prototype, 'error').get.call(p),
                     () => Object.getOwnPropertyDescriptor(PromiseRejectionEvent.prototype, 'promise').get.call(e)]) {
    try { bad(); return false; } catch (error) { if (!(error instanceof TypeError)) return false; }
  }
  return true;
}

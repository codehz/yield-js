// @uncaught ["inner"]

import { gc } from 'test';

export function run() {
  const error = new Proxy({}, { get() { throw 'proxy trap'; } });
  const log = [];
  globalThis.addEventListener('error', e => {
    gc();
    if (!(e instanceof ErrorEvent) || !e.isTrusted || !e.cancelable || e.bubbles ||
        e.target !== globalThis || e.error !== error || e.message !== 'Uncaught exception' ||
        !e.filename || e.lineno === 0 || e.colno === 0) throw 'bad event';
    log.push(e);
    throw 'inner';
  }, { once: true });
  onerror = function (message, filename, line, column, original) {
    const e = log[0];
    if (this !== globalThis || arguments.length !== 5 || original !== error ||
        message !== e.message || filename !== e.filename || line !== e.lineno || column !== e.colno) throw 'bad arguments';
    log.push('handler');
    return true;
  };
  if (reportError(error) !== undefined || log.length !== 2 || !log[0].defaultPrevented) throw 'not synchronous';
  try { reportError(); throw 'missing argument not rejected'; }
  catch (e) { if (!(e instanceof TypeError)) throw e; }
  try { new reportError(error); throw 'constructible'; }
  catch (e) { if (!(e instanceof TypeError)) throw e; }
  onerror = null;
  globalThis.addEventListener('error', e => e.preventDefault());
  reportError(undefined);
  const err = new TypeError('message');
  Object.defineProperty(err, 'name', { get() { throw 'getter'; } });
  globalThis.addEventListener('error', e => {
    if (e.error !== err || e.message !== 'Error: message') throw 'bad message';
  }, { once: true });
  reportError(err);
}

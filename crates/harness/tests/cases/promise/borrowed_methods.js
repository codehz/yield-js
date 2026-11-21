// @returns true

export function run() {
  let order = '';
  const marker = {};
  const receiver = {
    get constructor() {
      order += 'c';
      return { get [Symbol.species]() { order += 's'; return Promise; } };
    },
    get then() {
      order += 't';
      return function (a, b) {
        if (this !== receiver || b !== marker || (a !== undefined && a !== marker)) throw 'args';
        return 7;
      };
    },
  };
  if (Promise.prototype.catch.call(receiver, marker) !== 7 || order !== 't') return false;
  if (Promise.prototype.finally.call(receiver, marker) !== 7 || order !== 'tcst') return false;
  try { Promise.prototype.then.call(receiver); return false; }
  catch (e) { if (!(e instanceof TypeError) || order !== 'tcst') return false; }
  Object.defineProperty(receiver, 'then', { get() { throw marker; } });
  try { Promise.prototype.finally.call(receiver, marker); return false; }
  catch (e) { return e === marker && order === 'tcstcs'; }
}

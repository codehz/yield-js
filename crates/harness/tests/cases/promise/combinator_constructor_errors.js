// @returns true

export function run() {
  const result = {};
  const marker = {};
  let order = '';
  let throws = false;
  function C(executor) {
    order += 'c';
    executor(() => {}, function (error) {
      if (this !== undefined || error !== marker) throw 'bad rejection';
      order += 'r';
      if (throws) throw marker;
    });
    return result;
  }
  Object.defineProperty(C, 'resolve', { get() { order += 'g'; throw marker; } });
  const iterable = { get [Symbol.iterator]() { throw 'unexpected iterator'; } };
  for (const method of [Promise.all, Promise.allSettled, Promise.race, Promise.any]) {
    if (method.call(C, iterable) !== result) return false;
  }
  if (order !== 'cgrcgrcgrcgr') return false;
  throws = true;
  try { Promise.all.call(C, iterable); return false; }
  catch (error) { if (error !== marker || order !== 'cgrcgrcgrcgrcgr') return false; }
  function Missing() { return result; }
  Object.defineProperty(Missing, 'resolve', { get() { throw marker; } });
  try { Promise.all.call(Missing, iterable); return false; }
  catch (error) { return error instanceof TypeError; }
}

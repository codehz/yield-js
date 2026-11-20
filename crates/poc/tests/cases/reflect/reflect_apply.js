// @returns true

function check(a, b, ...extra) {
  return this.base === 10 && a === 2 && b === undefined && extra.length === 1 && extra[0] === 4;
}

export function run() {
  const apply = Reflect.apply;
  const receiver = {base: 10};
  if (!apply(check, receiver, [2, , 4])) return false;
  let trace = '';
  const list = Object.create({get 0() { trace += '0'; return 2; }});
  Object.defineProperty(list, 'length', {get() {
    trace += 'l';
    return {valueOf() { trace += 'v'; return '3.8'; }};
  }});
  Object.defineProperty(list, '2', {get() { trace += '2'; return 4; }});
  if (!apply(check, receiver, list) || trace !== 'lv02') return false;
  function missing(a, b) { return a === undefined && b === undefined && this === undefined; }
  if (!apply(missing, undefined, {}) || !apply(missing, undefined, {length: -1, 0: 1})) return false;
  let caught = 0;
  try { apply({}, null, []); } catch (e) { if (e instanceof TypeError) caught++; }
  try { apply(check, receiver, 'abc'); } catch (e) { if (e instanceof TypeError) caught++; }
  try { apply(check, receiver); } catch (e) { if (e instanceof TypeError) caught++; }
  const thrown = {};
  try { apply(() => { throw thrown; }, null, []); } catch (e) { if (e === thrown) caught++; }
  try { apply(check, receiver, {length: 1, get 0() { throw thrown; }}); }
  catch (e) { if (e === thrown) caught++; }
  return caught === 5;
}

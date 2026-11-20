// @returns true
export function run() {
  const a = {};
  const b = {};
  const m = new Map([[a, 1], [b, 2], [NaN, 3], [-0, 4]]);
  if (m.size !== 4 || m.get(a) !== 1 || m.has({}) || m.get(NaN) !== 3 || m.get(0) !== 4) return false;
  if (m.set(a, 5) !== m || m.get(a) !== 5 || m.keys().next().value !== a) return false;
  if (!m.delete(a) || m.delete(a) || m.get(a) !== undefined) return false;
  m.set(a, 6);
  if (m.keys().next().value !== b || m.values().next().value !== 2) return false;
  if (m[Symbol.iterator] !== m.entries || Map.prototype.constructor !== Map) return false;
  if (Object.getPrototypeOf(Map.prototype) !== Object.prototype) return false;
  const i = m.entries();
  if (i[Symbol.iterator]() !== i || i.next().value[0] !== b) return false;
  m.set(NaN, 7);
  if (i.next().value[1] !== 7 || 1 / i.next().value[0] !== Infinity) return false;
  if (m.clear() !== undefined || m.size !== 0) return false;
  m.set('new', 8);
  if (i.next().value[1] !== 8 || !i.next().done) return false;
  m.set('late', 9);
  if (!i.next().done) return false;
  const ctx = { log: '' };
  m.forEach(function (v, k, self) {
    if (self !== m) throw 'bad map receiver';
    this.log += k + v;
    if (k === 'new') {
      m.delete('late');
      m.set('end', 10);
    }
  }, ctx);
  return ctx.log === 'new8end10';
}

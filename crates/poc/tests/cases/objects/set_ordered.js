// @returns true
export function run() {
  const obj = {};
  const s = new Set([NaN, NaN, -0, 0, obj, obj]);
  if (s.size !== 3 || !s.has(NaN) || !s.has(-0) || !s.has(obj) || s.has({})) return false;
  if (s.add(obj) !== s || s.size !== 3 || !s.delete(NaN) || s.delete(NaN)) return false;
  if (s.keys !== s.values || s[Symbol.iterator] !== s.values) return false;
  if (Set.prototype.constructor !== Set || Object.getPrototypeOf(Set.prototype) !== Object.prototype) return false;
  const e = s.entries();
  if (e[Symbol.iterator]() !== e) return false;
  const zero = e.next().value;
  if (1 / zero[0] !== Infinity || zero[1] !== 0) return false;
  const dup = e.next().value;
  if (dup[0] !== obj || dup[1] !== obj) return false;
  if (s.clear() !== undefined || s.size !== 0) return false;
  s.add('a').add('b');
  if (e.next().value[0] !== 'a') return false;
  const ctx = { log: '' };
  s.forEach(function (v, k, self) {
    if (k !== v || self !== s) throw 'bad set arguments';
    this.log += v;
    if (v === 'a') {
      s.clear();
      s.add('c');
    }
  }, ctx);
  return ctx.log === 'ac' && e.next().value[0] === 'c' && e.next().done && s.size === 1;
}

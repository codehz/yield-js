// @returns true
export function run() {
  let closed = 0;
  const iterable = {
    [Symbol.iterator]() {
      let index = 0;
      return {
        next() {
          index++;
          return index === 1 ? { value: { get 0() { return 'k'; }, 1: 2 }, done: false } : { done: true };
        },
        return() { closed++; return {}; }
      };
    }
  };
  const m = new Map(iterable);
  if (m.get('k') !== 2 || closed !== 0 || new Map(null).size !== 0 || new Set(undefined).size !== 0) throw 'custom or nullish iterable';
  if (new Set('a😀a').size !== 2 || new Map(m).get('k') !== 2) throw 'string or Map iterable';
  function typeError(f) {
    try { f(); } catch (e) { return e instanceof TypeError; }
    return false;
  }
  const s = new Set();
  const ms = Object.getOwnPropertyDescriptor(Map.prototype, 'size');
  const ss = Object.getOwnPropertyDescriptor(Set.prototype, 'size');
  if (ms.enumerable || ss.enumerable || !ms.configurable || ms.set !== undefined) throw 'size descriptor';
  if (Reflect.apply(ms.get, m, []) !== 1 || Reflect.apply(ss.get, s, []) !== 0) throw 'getter invocation';
  const bad = {
    [Symbol.iterator]() {
      return {
        next() { return { value: 'x', done: false }; },
        return() { closed++; throw 'preserve the earlier error'; }
      };
    }
  };
  if (!typeError(() => Map()) || !typeError(() => Set())) throw 'new required';
  if (!typeError(() => new Map(3)) || !typeError(() => new Set({}))) throw 'non-iterable';
  if (!typeError(() => new Map(bad)) || closed !== 1) throw 'invalid entry close';
  if (!typeError(() => Map.prototype.size) || !typeError(() => Set.prototype.size)) throw 'prototype receiver';
  if (!typeError(() => Reflect.apply(ms.get, s, [])) || !typeError(() => Reflect.apply(ss.get, m, []))) throw 'getter receiver';
  if (!typeError(() => Reflect.apply(Map.prototype.get, s, ['k'])) || !typeError(() => Reflect.apply(Set.prototype.add, m, [1]))) throw 'method receiver';
  if (!typeError(() => Reflect.apply(m.keys().next, s.keys(), [])) || !typeError(() => s.forEach(1))) throw 'iterator receiver or callback';
  return true;
}

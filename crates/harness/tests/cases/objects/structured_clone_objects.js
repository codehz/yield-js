// @returns true
import { expect, gc } from 'test';

export function run() {
  const shared = { label: 'shared' };
  const sparse = new Array(5);
  sparse[2] = shared;
  const source = { shared, sparse, map: new Map(), set: new Set() };
  source.self = source;
  source.map.set(shared, source);
  source.set.add(shared);
  Object.defineProperty(source, 'readonly', { value: shared, enumerable: true });
  Object.defineProperty(source, '__proto__', { value: shared, enumerable: true });
  Object.defineProperty(source, 'hidden', { get() { throw 'nonenumerable key read'; } });
  source[Symbol('ignored')] = () => {};
  const copy = structuredClone(source);
  expect(copy === source).toBe(false);
  expect(copy.shared === shared).toBe(false);
  expect(copy.self).toBe(copy);
  expect(copy.map.get(copy.shared)).toBe(copy);
  expect(copy.map.has(shared)).toBe(false);
  expect(copy.set.has(copy.shared)).toBe(true);
  expect(copy.set.has(shared)).toBe(false);
  expect(copy.sparse.length).toBe(5);
  expect(Object.keys(copy.sparse).join(',')).toBe('2');
  expect(copy.sparse[2]).toBe(copy.shared);
  expect(Object.getPrototypeOf(copy)).toBe(Object.prototype);
  expect(Object.hasOwn(copy, '__proto__')).toBe(true);
  expect(copy.__proto__).toBe(copy.shared);
  expect(Object.hasOwn(copy, 'hidden')).toBe(false);
  expect(Object.getOwnPropertySymbols(copy).length).toBe(0);
  const descriptor = Object.getOwnPropertyDescriptor(copy, 'readonly');
  expect(descriptor.value).toBe(copy.shared);
  expect(descriptor.writable && descriptor.enumerable && descriptor.configurable).toBe(true);

  const order = [];
  const map = new Map();
  // These pending objects are reachable only through the collection before it is cleared.
  const key = { get id() { order.push('key'); return 1; } };
  map.set(key, {
    get id() {
      order.push('value');
      map.clear();
      map.set('added', 'too late');
      gc();
      return 2;
    },
  });
  map.set({ id: 3 }, { id: 4 });
  const ordered = {
    get first() {
      order.push('first');
      delete this.removed;
      this.added = 'too late';
      return map;
    },
    get removed() { throw 'deleted getter read'; },
    get last() { order.push('last'); gc(); return 5; },
  };
  const observed = structuredClone(ordered);
  expect(order.join(',')).toBe('first,key,value,last');
  expect(Object.keys(observed).join(',')).toBe('first,last');
  expect(observed.first.size).toBe(2);
  const entries = observed.first.entries();
  const first = entries.next().value;
  const second = entries.next().value;
  expect(first[0].id).toBe(1);
  expect(first[1].id).toBe(2);
  expect(second[0].id).toBe(3);
  expect(second[1].id).toBe(4);
  expect(observed.last).toBe(5);

  let chain = { end: true };
  for (let i = 0; i < 10000; i++) chain = { next: chain };
  let cursor = structuredClone(chain);
  for (let i = 0; i < 10000; i++) {
    if (cursor === chain) throw 'deep clone reused a source node';
    cursor = cursor.next;
    chain = chain.next;
  }
  expect(cursor === chain).toBe(false);
  expect(cursor.end).toBe(true);
  return true;
}

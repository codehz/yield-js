// @fulfilled undefined

import { expect } from 'test';

function checkSubclass(value, Sub, Base) {
  expect(Object.getPrototypeOf(value)).toBe(Sub.prototype);
  expect(value instanceof Sub).toBe(true);
  expect(value instanceof Base).toBe(true);
}

function expectTypeError(fn) {
  let name;
  try { fn(); } catch (error) { name = error.name; }
  expect(name).toBe('TypeError');
}

export async function run() {
  class List extends Array {}
  const list = new List(3);
  checkSubclass(list, List, Array);
  expect(Array.isArray(list)).toBe(true);
  expect(list.length).toBe(3);
  expect(0 in list).toBe(false);
  list[4] = 'item';
  expect(list.length).toBe(5);
  list.length = 2;
  expect(4 in list).toBe(false);

  class Table extends Map {}
  const table = new Table([['key', 1]]);
  checkSubclass(table, Table, Map);
  expect(table.get('key')).toBe(1);
  expect(table.set('other', 2)).toBe(table);
  expect(table.size).toBe(2);

  class Unique extends Set {}
  const unique = new Unique([1, 1, 2]);
  checkSubclass(unique, Unique, Set);
  expect(unique.size).toBe(2);
  expect(unique.has(2)).toBe(true);

  class WeakTable extends WeakMap {}
  const key = {};
  const weak = new WeakTable([[key, 3]]);
  checkSubclass(weak, WeakTable, WeakMap);
  expect(weak.get(key)).toBe(3);

  class Pattern extends RegExp {}
  const pattern = new Pattern('a+', 'g');
  checkSubclass(pattern, Pattern, RegExp);
  expect(pattern.exec('baa')[0]).toBe('aa');
  expect(pattern.lastIndex).toBe(3);

  class Text extends String {}
  const text = new Text('abc');
  checkSubclass(text, Text, String);
  expect(text.valueOf()).toBe('abc');
  expect(text[1]).toBe('b');
  expect(text.length).toBe(3);

  class Count extends Number {}
  const count = new Count(7);
  checkSubclass(count, Count, Number);
  expect(count.valueOf()).toBe(7);

  class Flag extends Boolean {}
  const flag = new Flag(false);
  checkSubclass(flag, Flag, Boolean);
  expect(flag.valueOf()).toBe(false);

  class Task extends Promise {}
  let unrelated;
  const task = new Task(resolve => {
    unrelated = new Promise(resolveOther => resolveOther(8));
    resolve(9);
  });
  checkSubclass(task, Task, Promise);
  expect(Object.getPrototypeOf(unrelated)).toBe(Promise.prototype);
  expect(await unrelated).toBe(8);
  expect(await task).toBe(9);

  class Record extends Object {}
  const existing = { marker: true };
  const record = new Record(existing);
  checkSubclass(record, Record, Object);
  expect(record === existing).toBe(false);
  expect(Object.getPrototypeOf(existing)).toBe(Object.prototype);
  expect(new Object(existing)).toBe(existing);

  // Proxy keeps the target's prototype and its existing target-kind restrictions.
  const proxy = Reflect.construct(Proxy, [list, {}], Table);
  expect(Object.getPrototypeOf(proxy)).toBe(List.prototype);
  expect(proxy instanceof Table).toBe(false);
  proxy[2] = 'through proxy';
  expect(list[2]).toBe('through proxy');
  expect(list.length).toBe(3);
  expectTypeError(() => Reflect.construct(Proxy, [table, {}], List));
}

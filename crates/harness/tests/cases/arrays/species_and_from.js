// @returns undefined

import { expect, gc } from 'test';

export function run() {
  const log = [];
  const proto = { set 0(value) { throw 'inherited setter'; } };
  function Species(n) { log.push(n); return Object.create(proto); }
  const source = [1,,3];
  source.constructor = { [Symbol.species]:Species };
  const sliced = source.slice();
  expect(sliced.length).toBe(3);
  expect(Object.hasOwn(sliced, 1)).toBe(false);
  expect(sliced[0]).toBe(1);
  const mapped = source.map(v => { gc(); return v*2; });
  expect(mapped[2]).toBe(6);
  expect(Object.hasOwn(mapped, 'length')).toBe(false);
  const deleted = source.splice(0,2);
  expect(deleted.length).toBe(2);
  expect(Object.hasOwn(deleted, 1)).toBe(false);
  expect(log.join()).toBe('3,3,2');
  expect(Array[Symbol.species]).toBe(Array);
  expect([new Proxy([,4],{})].flat().join()).toBe('4');
  const keep = [2];
  keep[Symbol.isConcatSpreadable] = false;
  const spread = {length:3,0:7,2:9,[Symbol.isConcatSpreadable]:true};
  const concat = [1].concat(keep,spread);
  expect(concat.length).toBe(5);
  expect(concat[1]).toBe(keep);
  expect(concat[2]).toBe(7);
  expect(Object.hasOwn(concat, 3)).toBe(false);
  expect(concat[4]).toBe(9);
  expect(Array.prototype.concat.call({get length() { throw 'not spread'; }}).length).toBe(1);

  function Target(n) { log.push('construct:'+arguments.length+':'+n); return Object.create(proto); }
  Object.defineProperty(Target,Symbol.species,{get() { throw 'static species'; }});
  log.length = 0;
  expect(Array.of.call(Target,4,5).length).toBe(2);
  const like = Array.from.call(Target,{length:2,1:8});
  expect(like.length).toBe(2);
  expect(Object.hasOwn(like, 0)).toBe(true);
  expect(like[1]).toBe(8);
  let closed = 0;
  let fail = false;
  const iterable = {
    [Symbol.iterator]() {
      log.push('iterator');
      let i = 0;
      return {
        next() { if (fail) throw 'next'; return {done:i===1, value:++i}; },
        return() { closed++; throw 'close'; }
      };
    }
  };
  const result = Array.from.call(Target,iterable, x => x+1);
  expect(result.length).toBe(1);
  expect(result[0]).toBe(2);
  expect(log.join()).toBe('construct:1:2,construct:1:2,construct:0:undefined,iterator');
  let error;
  try { Array.from(iterable, () => { throw 'map'; }); } catch (e) { error = e; }
  expect(error).toBe('map');
  expect(closed).toBe(1);
  function Blocked() { return new Proxy(Object.preventExtensions({}),{}); }
  try { Array.from.call(Blocked,iterable); } catch (e) { error = e.name; }
  expect(error).toBe('TypeError');
  expect(closed).toBe(2);
  fail = true;
  try { Array.from(iterable); } catch (e) { error = e; }
  expect(error).toBe('next');
  expect(closed).toBe(2);
}

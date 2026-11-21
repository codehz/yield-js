// @returns undefined

import { expect, gc } from 'test';

export function run() {
  const input = [undefined,'b',,'a'];
  const copy = input.toSorted();
  expect(input[1]).toBe('b');
  expect(copy.join()).toBe('a,b,,');
  expect(copy.hasOwnProperty(3)).toBe(true);
  expect(input.sort()).toBe(input);
  expect(input.join()).toBe('a,b,,');
  expect(input.hasOwnProperty(2)).toBe(true);
  expect(input.hasOwnProperty(3)).toBe(false);
  expect([{id:1,k:2},{id:2,k:1},{id:3,k:2}].toSorted((a,b) => a.k-b.k).map(x => x.id).join()).toBe('2,1,3');

  const unsorted = [3,1,2];
  let error;
  try { unsorted.sort(() => { throw 'compare'; }); } catch (e) { error = e; }
  expect(error).toBe('compare');
  expect(unsorted.join()).toBe('3,1,2');
  const garbage = [{n:3},{n:2},{n:1},{n:0}];
  const sorted = garbage.toSorted((a,b) => { garbage.length = 0; gc(); return a.n-b.n; });
  expect(sorted.map(x => x.n).join()).toBe('0,1,2,3');
  const p = new Proxy({}, { get() { throw 'string'; } });
  try { [p,0].sort(); } catch (e) { error = e; }
  expect(error).toBe('string');
  const bmp = new Proxy({ toString() { return '\ue000'; } }, {});
  const astral = new Proxy({ toString() { return '\u{10000}'; } }, {});
  expect([bmp,astral].sort()[0]).toBe(astral);

  const reads = [];
  const source = [1,2,3];
  source.constructor = { get [Symbol.species]() { throw 'species'; } };
  function install(i) {
    Object.defineProperty(source,i,{get() { reads.push(i); return i+1; }, configurable:true});
  }
  for (const i of [0,1,2]) install(i);
  expect(source.toReversed().join()).toBe('3,2,1');
  expect(reads.join()).toBe('2,1,0');
  expect(source.toSorted().join()).toBe('1,2,3');
  Object.defineProperty(source,1,{get() { throw 'skip'; }});
  expect(source.with(-2,9).join()).toBe('1,9,3');
  expect(source.toSpliced(1,1,8,9).join()).toBe('1,8,9,3');
  expect([1,,3].toSpliced().hasOwnProperty(1)).toBe(true);
  expect([1,2,3].toSpliced(1).join()).toBe('1');
  expect([1,2,3].toSpliced(1,undefined).join()).toBe('1,2,3');
  try { source.with(3,9); } catch (e) { error = e.name; }
  expect(error).toBe('RangeError');
}

// @returns true
import { assert, gc } from 'test';

export function run() {
  let calls = '';
  const thrown = {};
  for (const name of ['count', 'countReset', 'time', 'timeLog', 'timeEnd']) {
    const f = console[name];
    assert(f({ toString() { calls += name + ';'; return 'label'; } }) === undefined);
    let caught;
    try { f({ toString() { throw thrown; } }); } catch (e) { caught = e; }
    assert(caught === thrown);
    caught = undefined;
    try { f(Symbol('label')); } catch (e) { caught = e; }
    assert(caught instanceof TypeError);
  }
  assert(calls === 'count;countReset;time;timeLog;timeEnd;');
  calls = '';
  const first = { toString() { calls += 'first;'; gc(); return '12px'; } };
  const second = { toString() { calls += 'second;'; throw thrown; } };
  const third = { toString() { calls += 'third;'; return 'later'; } };
  let caught;
  try { console.log('%d %s %s', first, second, third); } catch (e) { caught = e; }
  assert(caught === thrown && calls === 'first;second;');
  console.log('%q', third);
  assert(calls === 'first;second;');
  for (const value of [Symbol(), Symbol('abc'), Symbol.for('def'), Symbol.isConcatSpreadable]) {
    console.log(value);
    console.log('%s %d %i %f', value, value, value, value);
  }
  const sparse = new Array(10000000);
  sparse[9999999] = 'last';
  console.log(sparse, new Array(150).fill('x'), new Uint8Array(10000000));
  return true;
}

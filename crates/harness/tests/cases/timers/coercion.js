// @advance 0
// @advance 2
// @fulfilled "signed@0,negative@0,undefined@0,nan@0,infinity@0,wrapped@2"

import { assert } from 'test';

function expectThrow(call, expected) {
  try { call(); } catch (error) {
    assert(expected === TypeError ? error instanceof TypeError : error === expected,
      'coercion exception');
    return;
  }
  assert(false, 'expected conversion to throw');
}

export function run() {
  const marker = {};
  const throwing = { valueOf() { throw marker; } };
  const stamps = [];
  const stamp = label => () => stamps.push(label + '@' + Date.now());
  for (const set of [setTimeout, setInterval]) {
    expectThrow(() => set(stamp('unexpected'), throwing), marker);
    expectThrow(() => set(stamp('unexpected'), 1n), TypeError);
    expectThrow(() => set(stamp('unexpected'), Symbol()), TypeError);
  }
  for (const clear of [clearTimeout, clearInterval]) {
    expectThrow(() => clear(throwing), marker);
    expectThrow(() => clear(1n), TypeError);
    expectThrow(() => clear(Symbol()), TypeError);
  }

  return new Promise(resolve => {
    const canceled = setTimeout(stamp('not canceled'), 0);
    let conversions = 0;
    clearInterval({ valueOf() { conversions++; return canceled + 2 ** 32 + 0.75; } });
    assert(conversions === 1, 'ID conversion occurs exactly once');

    setTimeout(stamp('signed'), 2 ** 31);
    setTimeout(stamp('negative'), -1.9);
    setTimeout(stamp('undefined'));
    setTimeout(stamp('nan'), NaN);
    setTimeout(stamp('infinity'), Infinity);
    setTimeout(stamp('wrapped'), { valueOf() { return 2 ** 32 + 2.9; } });
    setTimeout(() => resolve(stamps.join(',')), 2);
  });
}

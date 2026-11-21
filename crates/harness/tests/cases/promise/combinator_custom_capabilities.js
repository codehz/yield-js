// @returns true

export function run() {
  const result = {};
  const calls = [];
  const slots = [];
  const resolve = value => { calls.push(value); return 7; };
  const reject = reason => { calls.push(reason); return 8; };
  function C(executor) { executor(resolve, reject); return result; }
  C.resolve = function (value) {
    if (this !== C) throw 'receiver';
    return { then(a, b) { slots.push([a, b, value]); } };
  };
  if (Promise.race.call(C, [1]) !== result || slots[0][0] !== resolve || slots[0][1] !== reject) return false;
  C.resolve = value => ({ then(a, b) {
    if (a !== resolve || b !== reject || a(value) !== 7 || a(value + 1) !== 7 || b(9) !== 8) throw 'race callbacks';
  } });
  if (Promise.race.call(C, [1]) !== result || calls.join(',') !== '1,2,9') return false;
  C.resolve = value => ({ then(a, b) { slots.push([a, b, value]); } });
  if (Promise.all.call(C, [1, 2]) !== result || Promise.all.call(C, [3, 4]) !== result) return false;
  if (slots[1][1] !== reject || slots[2][0](2) !== undefined || slots[2][0](99) !== undefined) return false;
  slots[4][0](4);
  if (slots[3][0](3) !== 7 || slots[1][0](1) !== 7 || slots[1][0](99) !== undefined) return false;
  if (calls[3].join(',') !== '3,4' || calls[4].join(',') !== '1,2') return false;

  if (Promise.allSettled.call(C, [5, 6]) !== result) return false;
  slots[6][1](6);
  slots[6][0](99);
  if (slots[5][0](5) !== undefined) return false;
  slots[5][1](99);
  const settled = calls[5];
  if (settled[0].status !== 'fulfilled' || settled[0].value !== 5 || settled[1].status !== 'rejected' || settled[1].reason !== 6) return false;

  if (Promise.any.call(C, [7, 8]) !== result || slots[7][0] !== resolve) return false;
  slots[8][1](8);
  slots[8][1](99);
  if (slots[7][1](7) !== 8 || !(calls[6] instanceof AggregateError) || calls[6].errors.join(',') !== '7,8') return false;

  C.resolve = value => ({ then(a) { a(value); a(99); } });
  if (Promise.all.call(C, [10, 11]) !== result || calls[7].join(',') !== '10,11') return false;
  if (Promise.all.call(C, []) !== result || calls[8].length !== 0) return false;
  if (Promise.allSettled.call(C, []) !== result || calls[9].length !== 0) return false;
  if (Promise.race.call(C, []) !== result || calls.length !== 10) return false;
  return Promise.any.call(C, []) === result && calls[10] instanceof AggregateError && calls[10].errors.length === 0;
}

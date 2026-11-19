// @pending ["wait:collections"]
// @restore
// @fulfill ["wait:collections", null]
// @fulfilled true
export async function run() {
  const key = {};
  const symbol = Symbol('key');
  const m = new Map([[key, symbol], ['gone', 0], [symbol, key], ['last', { marker: 7 }]]);
  const i = m.entries();
  if (i.next().value[0] !== key) return false;
  m.delete('gone');
  const j = new Set([key, symbol, 'last']).values();
  if (j.next().value !== key) return false;
  await wait('collections');
  if (m.get(key) !== symbol || m.get(symbol) !== key || m.size !== 3) return false;
  m.set('append', 9);
  const middle = i.next().value;
  const last = i.next().value;
  return middle[0] === symbol && middle[1] === key && last[0] === 'last' &&
    last[1].marker === 7 && i.next().value[1] === 9 && i.next().done &&
    j.next().value === symbol && j.next().value === 'last' && j.next().done;
}

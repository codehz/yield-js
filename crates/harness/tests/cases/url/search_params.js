// @returns true

export function run() {
  const params = new URLSearchParams('?x=1&&x=2&space=a+b&bad=%FF&flag');
  if (params.size !== 5 || params.get('x') !== '1' || params.get('missing') !== null) return false;
  if (params.getAll('x').join(',') !== '1,2' || params.get('space') !== 'a b') return false;
  if (params.toString() !== 'x=1&x=2&space=a+b&bad=%EF%BF%BD&flag=') return false;
  if (!params.has('x', '2') || params.has('x', '3')) return false;
  params.append('x', '2');
  params.delete('x', 2);
  params.set('x', 4);
  if (params.getAll('x').join(',') !== '4' || !params.has('x', undefined)) return false;
  params.set('new', undefined);
  params.delete('flag', undefined);
  if (params.get('new') !== 'undefined' || params.has('flag')) return false;
  if (new URLSearchParams('??a=a').toString() !== '%3Fa=a') return false;
  if (new URLSearchParams(null).toString() !== 'null=') return false;
  if (new URLSearchParams(undefined).size !== 0) return false;

  const record = { 2: 'n', name: true };
  Object.defineProperty(record, 'hidden', { value: 1 });
  if (new URLSearchParams(record).toString() !== '2=n&name=true') return false;
  const sequence = new URLSearchParams(new Set([new Set(['a', 'b']), ['a', 'c']]));
  if (sequence.toString() !== 'a=b&a=c' || new URLSearchParams(sequence).toString() !== 'a=b&a=c') return false;
  if (new URLSearchParams([['\uD800', '\uDC00']]).toString() !== '%EF%BF%BD=%EF%BF%BD') return false;
  const sorted = new URLSearchParams([['\uE000', 'late'], ['𐀀', 'first'], ['a', '1'], ['a', '2']]);
  sorted.sort();
  if ([...sorted.values()].join(',') !== '1,2,first,late') return false;

  const live = new URLSearchParams('a=1&b=2&c=3');
  if (live[Symbol.iterator] !== live.entries) return false;
  const entries = live.entries();
  if (entries[Symbol.iterator]() !== entries || entries.next().value.join(':') !== 'a:1') return false;
  live.delete('a');
  live.append('d', 4);
  if (entries.next().value.join(':') !== 'c:3' || entries.next().value.join(':') !== 'd:4') return false;
  if (!entries.next().done) return false;
  live.append('e', 5);
  if (entries.next().value.join(':') !== 'e:5') return false;
  if ([...live.keys()].join(',') !== 'b,c,d,e') return false;
  const thisArg = {};
  let calls = '';
  live.forEach(function (value, name, self) {
    if (this !== thisArg || self !== live) throw new Error('incorrect callback arguments');
    calls += name + value;
    if (name === 'b') {
      live.delete('b');
      live.append('f', 6);
    }
  }, thisArg);
  if (calls !== 'b2d4e5f6') return false;

  let errors = 0;
  try { URLSearchParams(); } catch (e) { if (e instanceof TypeError) errors++; }
  try { new URLSearchParams([['short']]); } catch (e) { if (e instanceof TypeError) errors++; }
  try { new URLSearchParams(['ab']); } catch (e) { if (e instanceof TypeError) errors++; }
  try { params.append('missing'); } catch (e) { if (e instanceof TypeError) errors++; }
  try { Reflect.apply(params.get, {}, ['x']); } catch (e) { if (e instanceof TypeError) errors++; }
  try { Reflect.apply(entries.next, {}, []); } catch (e) { if (e instanceof TypeError) errors++; }
  return errors === 6;
}

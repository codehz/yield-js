// @returns true
// Adapted from WPT fetch/api/headers/headers-basic.any.js and headers-get-set-cookie.any.js.

export function run() {
  const headers = new Headers([
    ['z-last', ' \t\r\n\xE9\xFF\t '],
    ['X-Duplicate', 'first'],
    ['SET-cookie', 'a=1; Expires=Wed, 21 Oct 2015 07:28:00 GMT'],
    ['x-duplicate', 'second'],
    ['Set-Cookie', 'b=2'],
    ['empty', '\n\t '],
  ]);
  if (Object.getPrototypeOf(headers) !== Headers.prototype) return false;
  class CustomHeaders extends Headers {}
  const custom = new CustomHeaders({ custom: 'value' });
  if (Object.getPrototypeOf(custom) !== CustomHeaders.prototype || custom.get('custom') !== 'value') return false;
  const cookie = 'a=1; Expires=Wed, 21 Oct 2015 07:28:00 GMT';
  if (headers.get('Z-LAST') !== '\xE9\xFF' || headers.get('empty') !== '') return false;
  if (!headers.has('EMPTY') || headers.get('missing') !== null || headers.has('missing')) return false;
  if (headers.get('x-duplicate') !== 'first, second' || headers.get('set-cookie') !== cookie + ', b=2') return false;
  const cookies = headers.getSetCookie();
  if (cookies.length !== 2 || cookies[0] !== cookie || cookies[1] !== 'b=2') return false;
  cookies[0] = 'changed';
  if (headers.getSetCookie()[0] !== cookie) return false;
  if ([...headers.keys()].join('|') !== 'empty|set-cookie|set-cookie|x-duplicate|z-last') return false;
  if ([...headers.values()].join('|') !== '|' + cookie + '|b=2|first, second|\xE9\xFF') return false;
  if (new Headers(headers).getSetCookie().length !== 2) return false;

  headers.set('x-Duplicate', 3);
  headers.append('X-Duplicate', undefined);
  headers.set('set-cookie', 'replacement');
  if (headers.get('x-duplicate') !== '3, undefined' || headers.getSetCookie().join('|') !== 'replacement') return false;
  headers.delete('X-DUPLICATE');
  headers.delete('SET-COOKIE');
  if (headers.has('x-duplicate') || headers.getSetCookie().length !== 0) return false;

  const record = Object.create({ inherited: 'no' });
  record['Z-Record'] = false;
  record['z-record'] = 4;
  Object.defineProperty(record, 'hidden', { value: 'no' });
  Object.defineProperty(record, Symbol('hidden'), { value: 'no' });
  const fromRecord = new Headers(record);
  if ([...fromRecord.keys()].join('|') !== 'z-record' || fromRecord.get('z-record') !== 'false, 4') return false;
  const fromSequence = new Headers(new Set([new Set(['a', '1']), ['A', 2]]));
  if (fromSequence.get('a') !== '1, 2') return false;
  return Headers.length === 0 && Headers.prototype.append.length === 2 &&
    Object.prototype.toString.call(headers) === '[object Headers]';
}

// @returns true
// Adapted from WPT fetch/api/headers/headers-errors.any.js and headers-iterators.any.js.

export function run() {
  function throwsTypeError(callback) {
    try { callback(); } catch (error) { return error instanceof TypeError; }
    return false;
  }
  const headers = new Headers({ b: '2', a: '1', c: '3' });
  if (headers.entries !== headers[Symbol.iterator]) return false;
  const iterator = headers.entries();
  if (Object.prototype.toString.call(iterator) !== '[object Headers Iterator]') return false;
  if (iterator[Symbol.iterator]() !== iterator || iterator.next().value.join(':') !== 'a:1') return false;
  headers.delete('a');
  headers.set('c', 'updated');
  if (iterator.next().value.join(':') !== 'c:updated' || !iterator.next().done) return false;
  headers.append('d', '4');
  if (iterator.next().value.join(':') !== 'd:4') return false;
  let calls = '';
  const thisArg = {};
  headers.forEach(function (value, name, receiver) {
    if (this !== thisArg || receiver !== headers) throw new Error('incorrect callback arguments');
    calls += name + ':' + value + ';';
    if (name === 'b') {
      headers.delete('b');
      headers.append('e', '5');
    }
  }, thisArg);
  if (calls !== 'b:2;d:4;e:5;') return false;
  const marker = {};
  try { headers.forEach(() => { throw marker; }); return false; }
  catch (error) { if (error !== marker) return false; }

  const invalid = [
    () => Headers(),
    () => new Headers(null),
    () => new Headers('ab'),
    () => new Headers(['ab']),
    () => new Headers([['one']]),
    () => new Headers([['three', '2', '3']]),
    () => new Headers({ [Symbol.iterator]: 42 }),
    () => new Headers({ [Symbol('key')]: 'value' }),
    () => headers.append('missing value'),
    () => headers.append('x', '\u0100'),
    () => headers.append('x', '\uD800'),
    () => headers.append('x', 'bad\u0000'),
    () => headers.append('x', 'bad\r\nfold'),
    () => headers.set('bad name', 'value'),
    () => headers.get(''),
    () => headers.has('é'),
    () => headers.delete(':'),
    () => headers.forEach({}),
    () => Reflect.apply(headers.get, {}, ['x']),
    () => Reflect.apply(iterator.next, {}, []),
  ];
  if (!invalid.every(throwsTypeError)) return false;

  let converted = false;
  if (!throwsTypeError(() => headers.append('bad name', { toString() { converted = true; return 'yes'; } }))) return false;
  if (!converted) return false;
  // Sequence item conversion (including the second outer entry) precedes arity validation.
  converted = false;
  if (!throwsTypeError(() => new Headers([['short'], ['ok', { toString() { converted = true; return 'yes'; } }]]))) return false;
  if (!converted) return false;

  let closes = '';
  const entries = {
    *[Symbol.iterator]() {
      try {
        yield {
          *[Symbol.iterator]() {
            try { yield '\u0100'; } finally { closes += 'inner;'; }
          },
        };
      } finally { closes += 'outer;'; }
    },
  };
  if (!throwsTypeError(() => new Headers(entries))) return false;
  return closes === 'inner;outer;';
}

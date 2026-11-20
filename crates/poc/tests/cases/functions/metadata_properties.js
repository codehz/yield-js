// @returns true

function example(a) {}

export function run() {
  const proto = Object.getPrototypeOf(example);
  if (Object.getPrototypeOf(Array) !== proto) return false;
  if (proto.name !== '' || proto.length !== 0 || Array.name !== 'Array' || Array.length !== 0 ||
      Object.keys.name !== 'keys' || Object.keys.length !== 0) return false;

  for (const fn of [example, proto, Array, Object.keys, example.bind(null)]) {
    if (!('name' in fn) || !('length' in fn) || !Reflect.has(fn, 'name') || !Reflect.has(fn, 'length')) return false;
    if (Object.keys(fn).includes('name') || Object.keys(fn).includes('length') ||
        Reflect.ownKeys(fn).includes('name') || Reflect.ownKeys(fn).includes('length')) return false;
    if (Object.getOwnPropertyDescriptor(fn, 'name') !== undefined ||
        Reflect.getOwnPropertyDescriptor(fn, 'length') !== undefined) return false;
    if (Reflect.set(fn, 'name', 'changed') || Reflect.set(fn, 'length', 99)) return false;
  }
  try { example.name = 'changed'; } catch (e) {
    return e instanceof TypeError && example.name === 'example' && example.length === 1;
  }
  return false;
}

// @returns true

function Target(a, b) { this.sum = a + b; return 77; }
function Other() { throw 'newTarget body must not run'; }

export function run() {
  const construct = Reflect.construct;
  const instance = construct(Target, {0: 20, 1: 22, length: '2'}, Other);
  if (instance.sum !== 42 || !(instance instanceof Other) || instance instanceof Target) return false;
  if (Object.getPrototypeOf(instance) !== Other.prototype) return false;
  if (!(construct(Target, [1, 2]) instanceof Target)) return false;
  const replacement = {};
  function ReturnsObject() { return replacement; }
  if (construct(ReturnsObject, [], Other) !== replacement) return false;
  const array = construct(Array, [3, 4], Other);
  if (!Array.isArray(array) || array.length !== 2 || array[1] !== 4 || !(array instanceof Other)) return false;
  const error = construct(TypeError, ['oops'], Other);
  if (error.message !== 'oops' || !(error instanceof Other) || error instanceof TypeError) return false;
  const argument = {};
  const object = construct(Object, [argument], Other);
  if (object === argument || !(object instanceof Other) || Object.getPrototypeOf(argument) !== Object.prototype) return false;
  const promise = construct(Promise, [resolve => resolve(42)], Other);
  if (!(promise instanceof Other) || !(construct(Promise, [resolve => resolve(42)]) instanceof Promise)) return false;
  Other.prototype = 0;
  function Fallback() {}
  Fallback.prototype = 0;
  if (Object.getPrototypeOf(construct(Fallback, [], Other)) !== Object.getPrototypeOf(new Fallback())) return false;
  if (Object.getPrototypeOf(construct(Array, [], Other)) !== Array.prototype) return false;
  return true;
}

// @returns true

export function run() {
  const symbol = Symbol("hidden");
  const object = {};
  Object.defineProperty(object, "data", {
    value: 7, writable: false, enumerable: true, configurable: false,
  });
  Object.defineProperty(object, symbol, {
    get: Object.keys, enumerable: false, configurable: true,
  });
  const descriptors = Object.getOwnPropertyDescriptors(object);
  const data = descriptors.data;
  const accessor = descriptors[symbol];
  return Object.keys(descriptors).length === 1 && data.value === 7 &&
    !data.writable && data.enumerable && !data.configurable &&
    accessor.get === Object.keys && accessor.set === undefined &&
    !accessor.enumerable && accessor.configurable;
}

// @fulfilled "ok"

export async function run() {
  const generator = function* () {};
  const asyncGenerator = async function* () {};

  const generatorProto = Object.getPrototypeOf(generator);
  if (generatorProto.prototype !== Object.getPrototypeOf(generator.prototype)) {
    return "generator function prototype";
  }
  const asyncGeneratorProto = Object.getPrototypeOf(asyncGenerator);
  if (asyncGeneratorProto.prototype !== Object.getPrototypeOf(asyncGenerator.prototype)) {
    return "async generator function prototype";
  }
  if (generatorProto === Object.getPrototypeOf(function () {}) ||
      asyncGeneratorProto === generatorProto ||
      Object.getPrototypeOf(generatorProto) !== Function.prototype ||
      Object.getPrototypeOf(asyncGeneratorProto) !== Function.prototype) {
    return "function prototype chain";
  }
  return "ok";
}

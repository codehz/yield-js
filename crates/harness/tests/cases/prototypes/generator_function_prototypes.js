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
  for (const [prototype, value, tag] of [
    [generatorProto, generator, "GeneratorFunction"],
    [generatorProto.prototype, generator(), "Generator"],
    [asyncGeneratorProto, asyncGenerator, "AsyncGeneratorFunction"],
    [asyncGeneratorProto.prototype, asyncGenerator(), "AsyncGenerator"],
  ]) {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, Symbol.toStringTag);
    if (!descriptor || descriptor.value !== tag || descriptor.writable ||
        descriptor.enumerable || !descriptor.configurable) {
      return tag + " descriptor";
    }
    if (Object.prototype.toString.call(value) !== "[object " + tag + "]") {
      return tag + " branding";
    }
  }
  return "ok";
}

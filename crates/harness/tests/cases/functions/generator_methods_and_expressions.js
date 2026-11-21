// @returns "3:ok:true:function:true"

export function run() {
  const holder = {
    value: 3,
    *method() {
      yield this.value;
    },
  };
  class Container {
    static *staticMethod() {
      yield "ok";
    }
  }
  const expression = function* () {
    yield true;
  };
  const fromMethod = holder.method().next();
  const fromStatic = Container.staticMethod().next();
  const fromExpression = expression().next();
  const iterator = holder.method();
  const self = iterator[Symbol.iterator]();
  return [
    fromMethod.value,
    fromStatic.value,
    fromExpression.value,
    typeof self.next,
    self === iterator,
  ].join(":");
}

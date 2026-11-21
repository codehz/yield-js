// @fulfilled "1|2|3|4"

export async function run() {
  async function* declared() {
    yield 1;
  }
  const expression = async function* () {
    yield 2;
  };
  const object = {
    async *method() {
      yield 3;
    },
  };
  class Holder {
    static async *method() {
      yield 4;
    }
  }
  const first = await declared().next();
  const second = await expression().next();
  const third = await object.method().next();
  const fourth = await Holder.method().next();
  return [first.value, second.value, third.value, fourth.value].join("|");
}

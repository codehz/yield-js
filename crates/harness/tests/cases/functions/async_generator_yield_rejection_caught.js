// @fulfilled "5:false"

export async function run() {
  async function* g() {
    try {
      yield Promise.reject(4);
    } catch (error) {
      // The rejected operand reaches the lexical catch, not the consumer.
      yield error + 1;
    }
  }
  const it = g();
  const first = await it.next();
  return `${first.value}:${first.done}`;
}

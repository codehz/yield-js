// @fulfilled "bad:undefined:true"

export async function run() {
  async function* g() {
    throw "bad";
  }
  const it = g();
  let caught;
  try {
    await it.next();
  } catch (error) {
    caught = error;
  }
  // An uncaught throw closes the generator; later requests still settle.
  const after = await it.next();
  return `${caught}:${after.value}:${after.done}`;
}

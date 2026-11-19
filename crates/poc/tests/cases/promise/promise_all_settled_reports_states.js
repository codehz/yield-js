// @fulfilled "fulfilled:1:rejected:no"

export async function run() {
  const states = await Promise.allSettled([1, Promise.reject("no")]);
  return states[0].status + ":" + states[0].value + ":" + states[1].status + ":" + states[1].reason;
}

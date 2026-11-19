// @pending ["wait:species"]
// @restore
// @fulfill ["wait:species", 5]
// @fulfilled true

let constructed;
function Species(executor) {
  constructed = new Promise(executor);
  return constructed;
}

export async function run() {
  const key = Symbol.species;
  const box = { [key]: Species };
  const source = (async () => await wait("species"))();
  source.constructor = box;
  const result = source.then(value => value + 1);
  const final = source.finally(() => 0);
  const used = result !== final && final === constructed;
  return used && (await result) === 6 && (await final) === 5
    && key === Symbol.species && box[Symbol.species] === Species;
}

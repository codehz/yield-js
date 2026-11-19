// @fulfilled "2|5|extra|1|;0|1|;;;;;;;;"

function keys(value) {
  let trace = "";
  for (const key in value) trace += key + "|";
  return trace + ";";
}

export async function run() {
  const sparse = [];
  sparse[5] = true;
  sparse[2] = undefined;
  sparse.extra = true;
  Object.setPrototypeOf(sparse, { 1: true });

  return keys(sparse) + keys("😀") + keys("") + keys(null) + keys(undefined)
    + keys(1) + keys(true) + keys(false) + keys(Symbol("primitive"));
}

// @args [2]
// @fulfilled "1a2b3a"

export async function run(value) {
  let out = "";
  let i = 1;
  while (i <= 3) {
    switch (i) {
      case value:
        out = out + i + "b";
        break;
      default:
        out = out + i + "a";
    }
    i = i + 1;
  }
  return out;
}

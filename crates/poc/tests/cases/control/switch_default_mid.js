// @args [1]
// @fulfilled "one;default;two"

export async function run(value) {
  let seen = "";
  switch (value) {
    case 1:
      seen = seen + "one;";
    default:
      seen = seen + "default;";
    case 2:
      seen = seen + "two";
      break;
    case 3:
      seen = seen + "three";
  }
  return seen;
}

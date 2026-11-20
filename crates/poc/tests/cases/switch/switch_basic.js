// @args [2]
// @fulfilled "two"

export async function run(value) {
  switch (value) {
    case 1:
      return "one";
    case 2:
      return "two";
    default:
      return "other";
  }
}

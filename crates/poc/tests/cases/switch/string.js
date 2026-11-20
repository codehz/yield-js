// @args ["b"]
// @fulfilled "matched-b"

export async function run(value) {
  switch (value) {
    case "a":
      return "matched-a";
    case "b":
      return "matched-b";
    default:
      return "other";
  }
}

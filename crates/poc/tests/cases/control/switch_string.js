// @args ["b"]
// @fulfilled "matched-b"

async function run(value) {
  switch (value) {
    case "a":
      return "matched-a";
    case "b":
      return "matched-b";
    default:
      return "other";
  }
}

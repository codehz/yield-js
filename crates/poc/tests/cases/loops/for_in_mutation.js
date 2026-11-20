// @fulfilled "first|tail|inherited|shared|"

export async function run() {
  const object = Object.create({ inherited: true, shared: true });
  object.first = true;
  object.shared = true;
  object.tail = true;

  let trace = "";
  for (let key in object) {
    trace += key + "|";
    if (key === "first") {
      delete object.shared;
      object.added = true;
    }
  }
  return trace;
}

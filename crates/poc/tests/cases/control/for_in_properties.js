// @fulfilled "2|10|first|01|accessor|0|inherited|"

export async function run() {
  const parent = { inherited: true, hidden: true, first: true, 0: true };
  const object = Object.create(parent);
  object.first = true;
  object[10] = true;
  object[2] = true;
  object["01"] = true;
  object[Symbol("ignored")] = true;
  Object.defineProperty(object, "hidden", { value: true });
  Object.defineProperty(object, "accessor", {
    enumerable: true,
    get() { throw new Error("for-in must not read values"); },
  });

  let trace = "";
  for (const key in object) trace += key + "|";
  return trace;
}

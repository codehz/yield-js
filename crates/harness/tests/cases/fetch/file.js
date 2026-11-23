// @fulfilled "ok"
// Based on FileAPI file/File-constructor.any.js WPT assertions.

function eq(actual, expected, label) {
  if (actual !== expected) throw new Error(label + ": " + actual + " !== " + expected);
}

export async function run() {
  eq(Object.getPrototypeOf(File), Blob, "constructor inheritance");
  eq(Object.getPrototypeOf(File.prototype), Blob.prototype, "prototype inheritance");
  eq(File.length, 2, "required constructor arguments");
  let missing = false;
  try { new File([]); } catch (error) { missing = error instanceof TypeError; }
  eq(missing, true, "required filename");

  const source = new Uint8Array([0, 65, 66, 0]);
  const file = new File([source.subarray(1, 3)], "x/y\udfff", { type: "TEXT/PLAIN", lastModified: -42.9 });
  source.fill(0);
  eq(file instanceof File && file instanceof Blob, true, "both brands in prototype chain");
  eq(Object.prototype.toString.call(file), "[object File]", "tag");
  eq(file.name, "x/y\ufffd", "USV filename");
  eq(file.type, "text/plain", "type inherited from Blob");
  eq(file.lastModified, -42, "lastModified truncates");
  eq(await file.text(), "AB", "copied typed-array window");
  const slice = file.slice(1);
  eq(slice instanceof File, false, "slice is only a Blob");
  eq(await slice.text(), "B", "File slice bytes");

  const before = Date.now();
  const defaultTime = new File([], "clock").lastModified;
  eq(defaultTime >= before && defaultTime <= Date.now(), true, "uses the Date clock");
  const name = Object.getOwnPropertyDescriptor(File.prototype, "name").get;
  let branded = false;
  try { name.call(new Blob()); } catch (error) { branded = error instanceof TypeError; }
  eq(branded, true, "File-only getter brand");
  class Derived extends File {}
  const derived = new Derived(["d"], "derived");
  eq(Object.getPrototypeOf(derived), Derived.prototype, "newTarget prototype");
  eq(await derived.text(), "d", "subclass receives File slots");
  return "ok";
}

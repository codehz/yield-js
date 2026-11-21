// @returns "ok"

let where = "";

function at(name) {
  where = name;
}

export function run() {
  try {
    return body();
  } catch (error) {
    if (typeof error === "string") {
      throw where + ": " + error;
    }
    throw where + ": " + ((error && error.message) || "thrown object");
  }
}

function body() {
  at("delete");
  const view = new Uint8Array(2);
  view[0] = 3;
  if (Reflect.deleteProperty(view, "0") !== false) {
    throw "an element is not deletable";
  }
  if (view[0] !== 3) {
    throw "delete must not clear an element";
  }
  if (Reflect.deleteProperty(view, "9") !== true) {
    throw "an absent index is deletable";
  }
  if (!("0" in view) || "9" in view || "-0" in view) {
    throw "canonical index presence";
  }
  at("prototype");
  Uint8Array.prototype["-0"] = 42;
  Uint8Array.prototype["01"] = 24;
  if (new Uint8Array(1)["-0"] !== undefined) {
    throw "a negative zero index must not read the prototype";
  }
  if (new Uint8Array(1)["01"] !== 24) {
    throw "a non-canonical string reads the prototype";
  }
  delete Uint8Array.prototype["-0"];
  delete Uint8Array.prototype["01"];
  at("ordinary");
  view["01"] = 7;
  if (view["01"] !== 7 || !("01" in view)) {
    throw "non-canonical strings are ordinary properties";
  }
  at("ownKeys");
  if (Reflect.ownKeys(new Uint8Array(2)).join(",") !== "0,1") {
    throw "own keys order";
  }
  at("descriptor");
  const described = Object.getOwnPropertyDescriptor(view, "0");
  if (
    described.value !== 3 ||
    described.writable !== true ||
    described.enumerable !== true ||
    described.configurable !== true
  ) {
    throw "element descriptor";
  }
  if (described === Object.getOwnPropertyDescriptor(new Uint8Array(1), "5")) {
    throw "an absent index has no descriptor";
  }
  if (Object.getOwnPropertyDescriptor(new Uint8Array(1), "5") !== undefined) {
    throw "an out-of-range index has no own descriptor";
  }
  at("define");
  Object.defineProperty(view, "0", { value: 9 });
  if (view[0] !== 9) {
    throw "defineProperty with only a value stores";
  }
  for (const descriptor of [{ configurable: false }, { enumerable: false }, { writable: false }]) {
    let rejected = false;
    try {
      Object.defineProperty(view, "0", descriptor);
    } catch (error) {
      rejected = error instanceof TypeError;
    }
    if (!rejected) {
      throw "an element descriptor cannot be restricted";
    }
  }
  let accessorRejected = false;
  try {
    Object.defineProperty(view, "0", { get: function () {} });
  } catch (error) {
    accessorRejected = error instanceof TypeError;
  }
  if (!accessorRejected) {
    throw "an element cannot become an accessor";
  }

  at("outOfRange");
  view[5] = 1;
  if ("5" in view) {
    throw "an out-of-range write must not create a property";
  }
  if (Reflect.set(view, "5", 1) !== true) {
    throw "an out-of-range write still reports success";
  }

  at("freeze");
  let frozen = false;
  try {
    Object.freeze(view);
  } catch (error) {
    frozen = error instanceof TypeError;
  }
  if (!frozen) {
    throw "a typed array with elements cannot be frozen";
  }
  const empty = new Uint8Array(0);
  Object.freeze(empty);
  if (Object.isFrozen(empty) !== true) {
    throw "an empty fixed typed array can be frozen";
  }

  at("preventExtensions");
  const resizable = new Uint8Array(new ArrayBuffer(2, { maxByteLength: 4 }));
  if (Reflect.preventExtensions(resizable) !== false) {
    throw "a length-tracking view cannot be made non-extensible";
  }

  at("iterate");
  const iterated = [];
  for (const key in new Uint8Array([1, 2])) {
    iterated.push(key);
  }
  if (iterated.join(",") !== "0,1") {
    throw "for-in visits every index";
  }
  if ([...new Uint8Array([4, 5])].join(",") !== "4,5") {
    throw "spread uses the array iterator";
  }
  if (Object.assign({}, new Uint8Array([1, 2]))[1] !== 2) {
    throw "Object.assign copies indices";
  }
  if (Object.keys(new Uint8Array(2)).length !== 2) {
    throw "Object.keys lists indices";
  }
  if (Array.prototype.join.call(new Uint8Array([1, 2]), "-") !== "1-2") {
    throw "array methods work on a typed array receiver";
  }
  if (Object.prototype.toString.call(new Float32Array(1)) !== "[object Float32Array]") {
    throw "Symbol.toStringTag";
  }

  at("proxy");
  const proxy = new Proxy(new Uint8Array([6]), {});
  if (proxy[0] !== 6 || Object.prototype.hasOwnProperty.call(proxy, "0") !== true) {
    throw "a proxy forwards element reads";
  }
  const shared = Object.getPrototypeOf(Uint8Array.prototype);
  const lengthGetter = Object.getOwnPropertyDescriptor(shared, "length").get;
  if (lengthGetter.call(new Uint8Array(3)) !== 3) {
    throw "a borrowed getter works on a real view";
  }
  if (lengthGetter.call(new Int8Array(2)) !== 2) {
    throw "the shared length getter accepts any TypedArray";
  }
  for (const receiver of [{}, new DataView(new ArrayBuffer(1))]) {
    let rejected = false;
    try {
      lengthGetter.call(receiver);
    } catch (error) {
      rejected = error instanceof TypeError;
    }
    if (!rejected) {
      throw "a borrowed getter rejects an wrong brand";
    }
  }
  let proxyBrand = false;
  try {
    lengthGetter.call(proxy);
  } catch (error) {
    proxyBrand = error instanceof TypeError;
  }
  if (!proxyBrand) {
    throw "a brand check must not see through a proxy";
  }

  at("metadata");
  const typed = new Int16Array(1);
  if (
    typed instanceof Int16Array !== true ||
    Object.getPrototypeOf(typed) !== Int16Array.prototype ||
    Object.getPrototypeOf(Int16Array) !== Object.getPrototypeOf(Uint8Array) ||
    Object.getPrototypeOf(Int16Array.prototype) !== Object.getPrototypeOf(Uint8Array.prototype)
  ) {
    throw "constructor inheritance";
  }
  if (
    Int16Array.name !== "Int16Array" ||
    Int16Array.length !== 3 ||
    Int16Array.BYTES_PER_ELEMENT !== 2 ||
    Int16Array.prototype.BYTES_PER_ELEMENT !== 2 ||
    typeof TypedArray !== "undefined"
  ) {
    throw "constructor metadata";
  }
  return "ok";
}

// @fulfilled true

export async function run() {
  try {
    await constructMethod();
  } catch (e) {
    return typeof e === "object" && e.name === "TypeError" && e.message === "value is not a constructor";
  }
  return false;
}

async function constructMethod() {
  const o = {
    m() {},
    C: function () { this.ok = true; }
  };
  if (!(new o.C()).ok) throw "function-valued property is not constructable";
  return new o.m();
}

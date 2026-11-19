// @rejected "TypeError: value is not a constructor"

async function run() {
  const o = {
    m() {},
    C: function () { this.ok = true; }
  };
  if (!(new o.C()).ok) throw "function-valued property is not constructable";
  return new o.m();
}

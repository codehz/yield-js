// @fulfilled true

export async function run() {
  const method = [].join;
  function join(source, separator) {
    source.join = method;
    return source.join(separator);
  }
  let order = "";
  const source = {
    get length() {
      order += "l";
      return { valueOf() { order += "v"; return " 2.9 "; } };
    },
    get 0() { order += "0"; return "a"; },
    get 1() { order += "1"; return "b"; },
    get 2() { throw "length was not truncated"; }
  };
  const sep = { toString() { order += "s"; return "-"; } };
  if (join(source, sep) !== "a-b" || order !== "lvs01") return false;
  if (join({ length: true, 0: "x" }) !== "x") return false;
  if (join({ length: "\uFEFF0x2", 0: "x", 1: "y" }) !== "x,y") return false;
  if (join({ length: "inf", get 0() { throw "invalid length"; } }) !== "") return false;

  const marker = {};
  try {
    join({ length: { valueOf() { throw marker; } } }, sep);
    return false;
  } catch (e) { if (e !== marker || order !== "lvs01") return false; }
  try {
    join({ length: "Infinity", get 0() { throw marker; } });
    return false;
  } catch (e) { if (e !== marker) return false; }
  try { join({ length: Symbol() }); return false; } catch (e) { return e.name === "TypeError"; }
}

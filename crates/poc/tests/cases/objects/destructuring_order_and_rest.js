// @fulfilled "RKCTGS|true:4:5:6:7:undefined:false:false:false|8:9:10:11"

export async function run() {
  let trace = "";
  const omit = Symbol("omit");
  const keep = Symbol("keep");
  const source = Object.create({ inherited: 4, dropped: 99 });
  Object.defineProperty(source, "hidden", { value: 99 });
  Object.defineProperty(source, omit, {
    enumerable: true,
    get() { trace += "G"; return 5; },
  });
  source.keep = 6;
  source[keep] = 7;
  const key = {
    [Symbol.toPrimitive]() { trace += "C"; return omit; },
  };
  const target = { set value(value) { trace += "S"; this.saved = value; } };
  let inherited;
  let rest;
  const result = ({
    inherited,
    [(trace += "K", key)]: (trace += "T", target).value,
    ...rest
  } = (trace += "R", source));

  const { values: [first = 8, ...tail], ...extra } = { values: [undefined, 9, 10], extra: 11 };
  return trace + "|" + (result === source) + ":" + inherited + ":" + target.saved + ":"
    + rest.keep + ":" + rest[keep] + ":" + rest[omit] + ":" + ("inherited" in rest)
    + ":" + ("dropped" in rest) + ":" + ("hidden" in rest)
    + "|" + first + ":" + tail[0] + ":" + tail[1] + ":" + extra.extra;
}

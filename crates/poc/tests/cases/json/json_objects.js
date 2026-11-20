// @returns true
export function run() {
  const hole = [1];
  hole.length = 3;
  delete hole[0];
  if (JSON.stringify(hole) !== "[null,null,null]") return false;
  const ordered = { b: 1, a: 2 };
  if (JSON.stringify(ordered) !== "{\"b\":1,\"a\":2}") return false;
  const hidden = { a: 1, skip() {}, [Symbol()]: 2 };
  hidden.gone = undefined;
  if (JSON.stringify(hidden) !== "{\"a\":1}") return false;
  const dated = { toJSON(key) { return key + "v"; } };
  if (JSON.stringify({ dated }) !== "{\"dated\":\"datedv\"}") return false;
  const replaced = JSON.stringify({ a: 1, b: 2 }, (key, value) => key === "b" ? undefined : value);
  if (replaced !== "{\"a\":1}") return false;
  const listed = JSON.stringify({ a: 1, b: 2, c: 3 }, ["c", "a", "c", 1, true]);
  if (listed !== "{\"c\":3,\"a\":1}") return false;
  if (JSON.stringify({ a: 1 }, null, 2) !== "{\n  \"a\": 1\n}") return false;
  if (JSON.stringify([1], null, "<>") !== "[\n<>1\n]") return false;
  const cycle = { a: 1 };
  cycle.self = cycle;
  try { JSON.stringify(cycle); return false; } catch (e) {
    if (!(e instanceof TypeError) || e.message.indexOf(".self") === -1) return false;
  }
  const throwing = { get boom() { throw "getter"; } };
  try { JSON.stringify(throwing); return false; } catch (e) { if (e !== "getter") return false; }
  const revived = JSON.parse("{\"a\":1,\"b\":[2,3]}", function (key, value) {
    if (key === "b") return undefined;
    if (key === "a") return value + 1;
    return value;
  });
  if (revived.a !== 2 || "b" in revived) return false;
  const round = JSON.parse(JSON.stringify({ z: "ok", n: [false, null] }));
  return round.z === "ok" && round.n[0] === false && round.n[1] === null;
}

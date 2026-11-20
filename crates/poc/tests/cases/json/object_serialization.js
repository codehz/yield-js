// @returns undefined

import { expect } from 'test';

export function run() {
  const hole = [1];
  hole.length = 3;
  delete hole[0];
  expect(JSON.stringify(hole)).toBe("[null,null,null]");
  const ordered = { b: 1, a: 2 };
  expect(JSON.stringify(ordered)).toBe("{\"b\":1,\"a\":2}");
  const hidden = { a: 1, skip() {}, [Symbol()]: 2 };
  hidden.gone = undefined;
  expect(JSON.stringify(hidden)).toBe("{\"a\":1}");
  const dated = { toJSON(key) { return key + "v"; } };
  expect(JSON.stringify({ dated })).toBe("{\"dated\":\"datedv\"}");
  const replaced = JSON.stringify({ a: 1, b: 2 }, (key, value) => key === "b" ? undefined : value);
  expect(replaced).toBe("{\"a\":1}");
  const listed = JSON.stringify({ a: 1, b: 2, c: 3 }, ["c", "a", "c", 1, true]);
  expect(listed).toBe("{\"c\":3,\"a\":1}");
  expect(JSON.stringify({ a: 1 }, null, 2)).toBe("{\n  \"a\": 1\n}");
  expect(JSON.stringify([1], null, "<>")).toBe("[\n<>1\n]");
  const cycle = { a: 1 };
  cycle.self = cycle;
  let cycleError;
  try {
    JSON.stringify(cycle);
  } catch (error) {
    cycleError = error;
  }
  expect(cycleError instanceof TypeError).toBe(true);
  expect(cycleError.message.indexOf(".self") !== -1).toBe(true);
  const throwing = { get boom() { throw "getter"; } };
  let getterError;
  try {
    JSON.stringify(throwing);
  } catch (error) {
    getterError = error;
  }
  expect(getterError).toBe("getter");
  const revived = JSON.parse("{\"a\":1,\"b\":[2,3]}", function (key, value) {
    if (key === "b") return undefined;
    if (key === "a") return value + 1;
    return value;
  });
  expect(revived.a).toBe(2);
  expect("b" in revived).toBe(false);
  const round = JSON.parse(JSON.stringify({ z: "ok", n: [false, null] }));
  expect(round.z).toBe("ok");
  expect(round.n[0]).toBe(false);
  expect(round.n[1]).toBe(null);
}

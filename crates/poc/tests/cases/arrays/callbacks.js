// @returns 0
export function run() {
  let a = Array(4);
  a[1] = 3;
  a[3] = 5;
  let state = {sum: 0};
  a.forEach(function(value, index, source) { if (source === a) this.sum += value + index; }, state);
  if (state.sum !== 12) return 1;
  let mapped = a.map((x, i) => x * i);
  if (mapped.length !== 4 || 0 in mapped || mapped[1] !== 3 || 2 in mapped || mapped[3] !== 15) return 2;
  if (a.filter((x) => x > 3).join() !== "5" || a.reduce((acc, x, i) => acc + x + i, 1) !== 13 || a.reduce((acc, x) => acc + x) !== 8) return 3;
  if (!a.some((x) => x === 5) || !a.every((x) => x > 0) || a.some(() => false) || a.every((x) => x < 5)) return 4;
  let nested = [2, undefined, [3]];
  let outer = [1, undefined, nested];
  delete nested[1];
  delete outer[1];
  let flat = outer.flat(2);
  if (flat.length !== 3 || flat.join() !== "1,2,3" || !Array.isArray([[[4]]].flat()[0])) return 5;
  if (a.indexOf(undefined) !== -1 || !a.includes(undefined) || ![NaN].includes(NaN)) return 6;
  if ([2, 3, 2].lastIndexOf(2, -2) !== 0 || a.find((x) => x === 5) !== 5 || a.findIndex((x) => x === undefined) !== 0) return 7;
  if (a.flatMap((x) => { let row = [x, undefined, x]; delete row[1]; return row; }).join() !== "3,3,5,5" || 1 in [1].concat(a)) return 8;
  let errors = 0;
  try { Array(2).reduce((acc, x) => x); } catch (e) { if (e instanceof TypeError) errors++; }
  try { a.map(() => { throw 42; }); } catch (e) { if (e === 42) errors++; }
  let closed = 0;
  let source = {[Symbol.iterator]() { return {next() { return {value: 1}; }, return() { closed++; throw 9; }}; }};
  try { Array.from(source, () => { throw 7; }); } catch (e) { if (e === 7) errors++; }
  return errors !== 3 ? 9 : closed !== 1 ? 10 : 0;
}

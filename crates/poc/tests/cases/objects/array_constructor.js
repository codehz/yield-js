// @returns true
export function run() {
  let holes = new Array(3);
  if (Array().length !== 0 || holes.length !== 3 || 0 in holes) return false;
  if (!Array.isArray(holes) || !Array.isArray(Array.prototype) || Array.isArray({length: 3})) return false;
  if (Array.prototype.constructor !== Array || Object.getPrototypeOf([]) !== Array.prototype || Object.getPrototypeOf(holes) !== Array.prototype) return false;
  if (Array("3")[0] !== "3" || new Array(1, 2).join() !== "1,2" || Array.of(3).length !== 1) return false;
  if (!(0 in Array.from(holes)) || Array.from("a😀").join("|") !== "a|😀") return false;
  let a = Array.from({0: 5, length: 2}, function(value, index) { return index === 0 ? value + this.delta : index; }, {delta: 2});
  if (a.join() !== "7,1") return false;
  let source = {[Symbol.iterator]() { return [4, 5].values(); }};
  if (Array.from(source).join() !== "4,5") return false;
  let count = 0;
  try { Array(-1); } catch (e) { if (e instanceof TypeError) count++; }
  try { new Array(1.5); } catch (e) { if (e instanceof TypeError) count++; }
  try { Array.from(null); } catch (e) { if (e instanceof TypeError) count++; }
  try { Array.from([], null); } catch (e) { if (e instanceof TypeError) count++; }
  return count === 4;
}

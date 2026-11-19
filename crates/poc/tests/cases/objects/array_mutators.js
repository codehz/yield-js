// @returns true
export function run() {
  let a = Array(3);
  a[1] = 4;
  if (a.push(7, 8) !== 5 || a.pop() !== 8 || a.shift() !== undefined) return false;
  if (a.length !== 3 || a[0] !== 4 || 1 in a || a[2] !== 7) return false;
  if (a.unshift(1, 2) !== 5 || a.join() !== "1,2,4,,7") return false;
  let removed = a.splice(-3, 2, 9);
  if (removed.length !== 2 || removed[0] !== 4 || 1 in removed || a.join() !== "1,2,9,7") return false;
  a.splice(1, 0, 6, 5);
  if (a.join() !== "1,6,5,2,9,7" || a.slice(-4, -1).join() !== "5,2,9") return false;
  if (a.splice(4).join() !== "9,7" || a.splice().length !== 0 || a.length !== 4) return false;
  if (Array(4).slice(1).length !== 3 || 0 in Array(2).slice()) return false;
  let filled = Array(3).fill(5);
  if (filled.join() !== "5,5,5" || !(0 in filled)) return false;
  let errors = 0;
  let frozen = Object.freeze([1, 2]);
  try { frozen.shift(); } catch (e) { if (e instanceof TypeError) errors++; }
  try { frozen.pop(); } catch (e) { if (e instanceof TypeError) errors++; }
  let locked = [];
  Object.defineProperty(locked, "length", {writable: false});
  try { locked.push(1); } catch (e) { if (e instanceof TypeError) errors++; }
  try { locked.unshift(); } catch (e) { if (e instanceof TypeError) errors++; }
  return errors === 4 && frozen.join() === "1,2" && locked.length === 0;
}

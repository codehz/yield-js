// @returns true

export function run() {
  const { pair: [first, second = 2] } = { pair: [1] };
  const values = [];
  const box = { total: 0 };
  for (const [left, right] of [[first, second], [3, 4]]) {
    values.push(left);
    box.total += right;
  }
  values[0]++;
  return first === 1 && second === 2 && values.length === 2 &&
    values[0] === 2 && values[1] === 3 && box.total === 6;
}

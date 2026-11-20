// @args [1, 2]
// @returns true

export function run(parameter, ...rest) {
  let local = 3;
  var variable = 4;
  local += 1;
  variable++;
  parameter = 6;
  rest = [7];
  arguments[0] = 8;
  try {
    throw 9;
  } catch (caught) {
    caught = 10;
    return local === 4 && variable === 5 && parameter === 6 && rest[0] === 7 &&
      arguments[0] === 8 && caught === 10;
  }
}

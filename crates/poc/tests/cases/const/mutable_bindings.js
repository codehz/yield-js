// @args [1, 2]
// @returns undefined

import { expect } from 'test';

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
    expect(local).toBe(4);
    expect(variable).toBe(5);
    expect(parameter).toBe(6);
    expect(rest[0]).toBe(7);
    expect(arguments[0]).toBe(8);
    expect(caught).toBe(10);
  }
}

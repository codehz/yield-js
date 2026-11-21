// @returns 11

export function run() {
  let closes = 0;
  function* g() {
    try {
      yield 1;
      yield 2;
    } finally {
      closes = closes + 1;
    }
  }
  let seen = 0;
  for (const value of g()) {
    seen = seen + value;
    break;
  }
  return closes * 10 + seen;
}

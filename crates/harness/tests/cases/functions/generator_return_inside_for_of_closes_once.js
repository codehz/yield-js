// @returns "11:true"

export function run() {
  let closes = 0;
  const source = {};
  source[Symbol.iterator] = function () {
    return {
      next() {
        return { done: false, value: 1 };
      },
      return(value) {
        closes = closes + 1;
        return { done: true, value: value };
      },
    };
  };
  function* g() {
    try {
      for (const value of source) {
        yield value;
        break;
      }
    } finally {
      closes = closes + 10;
    }
  }
  const it = g();
  it.next();
  const closed = it.return();
  return [closes, closed.done].join(":");
}

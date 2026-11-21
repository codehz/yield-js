// @fulfilled "C1IGNN34M|9:1234|56|7|undefined|true"

function Pair(a, b) {
  this.value = a * 10 + b;
}

function first(a) {
  return a;
}

export async function run() {
  let trace = "";
  const mark = (value) => {
    trace += value;
    return value;
  };
  const obj = {
    id: 9,
    get method() {
      trace += "C";
      return function (a, b, c, d) {
        trace += "M";
        return `${this.id}:${a}${b}${c}${d}`;
      };
    },
  };
  const custom = {
    [Symbol.iterator]() {
      trace += "I";
      let done = false;
      return {
        get next() {
          trace += "G";
          const iterator = this;
          return function () {
            trace += "N";
            if (this !== iterator) throw "wrong receiver";
            if (done) return { done: true };
            done = true;
            return { value: 2 };
          };
        },
        return() {
          throw "must not close an exhausted iterator";
        },
      };
    },
  };
  const result = obj.method?.(mark(1), ...custom, ...[], ...[mark(3)], mark(4));
  const empty = null?.method(...mark("skipped"));
  const bad = undefined?.(...mark("skipped"));
  const sparse = [];
  sparse.length = 1;
  return `${trace}|${result}|${new Pair(...[5, 6]).value}|${first(...[7])}|${first(...sparse)}|${empty === undefined && bad === undefined}`;
}

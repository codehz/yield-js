// @fulfilled "1234ITypeErrorGnoiter|1,2,3,4,undefined,9"

export async function run() {
  let order = "";
  const mark = (value) => {
    order += value;
    return value;
  };
  const custom = {
    [Symbol.iterator]() {
      order += "I";
      let remaining = 1;
      return {
        next() {
          if (remaining === 0) return { done: true };
          remaining -= 1;
          return { value: 9, done: false };
        },
        return() {
          order += "closed";
          return {};
        },
      };
    },
  };
  const values = [mark(1), ...[mark(2), mark(3)], mark(4), , ...custom];
  try {
    const ignored = [...null];
    order += ignored;
  } catch (error) {
    order += error.name;
  }
  try {
    const broken = {
      [Symbol.iterator]() {
        order += "G";
        throw "noiter";
      },
    };
    const ignored = [...broken];
    order += ignored;
  } catch (error) {
    order += error;
  }
  return (
    order +
    "|" +
    values[0] +
    "," +
    values[1] +
    "," +
    values[2] +
    "," +
    values[3] +
    "," +
    values[4] +
    "," +
    values[5]
  );
}

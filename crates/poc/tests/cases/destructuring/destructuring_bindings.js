// @fulfilled "2:3|4:5|6:7|8:9|10:11"

export const { pair: [left, right] } = { pair: [2, 3] };

function read({ values: [first, second = 5] }) {
  return first + ":" + second;
}

export async function run() {
  let trace = left + ":" + right + "|" + read({ values: [4] });
  for (const [a, { b }] of [[6, { b: 7 }]]) {
    trace += "|" + a + ":" + b;
  }
  let key;
  let tail;
  for ([key, ...tail] in { "89": true }) {
    trace += "|" + key + ":" + tail[0];
  }
  try {
    throw { data: [10, 11] };
  } catch ({ data: [a, b] }) {
    trace += "|" + a + ":" + b;
  }
  return trace;
}

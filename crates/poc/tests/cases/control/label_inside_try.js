// @fulfilled 5

async function run() {
  let n = 0;
  try {
    done: {
      n = 1;
      break done;
      n = 8;
    }
    n++;
  } finally {
    n += 3;
  }
  return n;
}

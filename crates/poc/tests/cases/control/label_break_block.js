// @fulfilled 4

export async function run() {
  let n = 0;
  done: {
    while (true) {
      switch (1) {
        case 1:
          n = 3;
          break done;
      }
      n = 8;
      break;
    }
    n = 9;
  }
  done: {
    n++;
  }
  return n;
}

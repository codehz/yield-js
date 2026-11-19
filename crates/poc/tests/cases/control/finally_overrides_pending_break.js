// @fulfilled 2

async function run() {
  let i = 0;
  while (i < 2) {
    i++;
    try {
      break;
    } finally {
      if (i === 1) continue;
    }
  }
  return i;
}

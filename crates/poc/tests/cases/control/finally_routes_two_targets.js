// @fulfilled "ioio"

async function run() {
  let s = "";
  outer: for (let i = 0; i < 3; i++) {
    try {
      try {
        if (i === 0) continue outer;
        break outer;
      } finally {
        s += "i";
      }
    } finally {
      s += "o";
    }
  }
  return s;
}

// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 0]
// @fulfilled "f"

async function run() {
  let s = "";
  done: {
    try {
      break done;
    } finally {
      await wait("a");
      while (true) {
        break;
      }
      s += "f";
    }
    s += "x";
  }
  return s;
}

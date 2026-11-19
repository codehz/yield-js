// @fulfilled "iso"

async function run() {
  let s = "";
  try {
    switch (1) {
      case 1:
        try {
          throw 0;
        } catch (e) {
          break;
        } finally {
          s += "i";
        }
    }
    s += "s";
  } finally {
    s += "o";
  }
  return s;
}

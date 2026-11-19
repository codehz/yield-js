// @args [3]
// @fulfilled 7

async function run(value) {
  let total = 0;
  switch (value) {
    case 1:
      total = total + 1;
    case 2:
      total = total + 2;
    case 3:
      total = total + 3;
    case 4:
      total = total + 4;
  }
  return total;
}

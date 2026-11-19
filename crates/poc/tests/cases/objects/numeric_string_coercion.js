// @fulfilled true

async function run() {
  return +"foo" !== +"foo" && +"0x10" === 16 && +"0b1010" === 10 && +"0oZ" !== +"0oZ" && ("foo" | 0) === 0;
}

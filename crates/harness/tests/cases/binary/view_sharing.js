// @returns 65332

export function run() {
  const buffer = new ArrayBuffer(8);
  const view = new DataView(buffer);
  if (ArrayBuffer.isView(view) !== true) {
    throw "DataView is not a view";
  }
  if (view.buffer !== buffer || view.byteOffset !== 0 || view.byteLength !== 8) {
    throw "DataView record";
  }
  view.setUint16(1, 0x1234);
  const bytes = new Uint8Array(buffer);
  if (bytes[1] !== 0x12 || bytes[2] !== 0x34) {
    throw "DataView default endian";
  }
  if (view.getUint16(1, true) !== 0x3412) {
    throw "DataView little endian read";
  }
  bytes[1] = 0xff;
  if (view.getUint8(1) !== 0xff) {
    throw "views do not alias";
  }
  if (new ArrayBuffer(4).byteLength !== 4) {
    throw "ArrayBuffer byteLength";
  }
  return view.getUint16(1);
}

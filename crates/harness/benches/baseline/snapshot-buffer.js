import { wait } from "test";

export async function run() {
    const size = 8 * 1024 * 1024;
    const buffer = new ArrayBuffer(size);
    const bytes = new Uint8Array(buffer);
    const tail = new DataView(buffer, size - 8, 8);
    bytes[0] = 17;
    tail.setUint8(7, 29);

    const result = await wait("baseline-buffer");
    if (bytes.buffer !== tail.buffer || bytes.byteLength !== size || tail.byteLength !== 8) return -1;
    if (bytes[0] !== 17 || bytes[size - 1] !== 29) return -2;
    tail.setUint8(0, result);
    return bytes[size - 8] + tail.getUint8(7);
}

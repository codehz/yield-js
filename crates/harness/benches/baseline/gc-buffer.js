export function run(marker) {
    const bytes = new Uint8Array(4 * 1024 * 1024);
    // Touch the backing storage, not just its virtually allocated address range.
    for (let offset = 0; offset < bytes.length; offset += 4096) {
        bytes[offset] = marker;
    }
    return bytes[bytes.length - 4096];
}

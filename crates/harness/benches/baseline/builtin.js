export function run() {
    let total = 0;
    for (let i = 0; i < 2048; i++) {
        total += Math.abs(-i);
        total += Math.floor(i + 0.5);
        total += Math.max(i, 7);
        if (Number.isInteger(i)) total += 1;
        if (Number.isFinite(i)) total += 2;
    }
    return total;
}

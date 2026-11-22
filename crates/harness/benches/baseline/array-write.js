export function run() {
    const values = [];
    for (let i = 0; i < 1024; i++) values.push(0);
    for (let round = 0; round < 32; round++) {
        for (let i = 0; i < 1024; i++) values[i] = i + round;
    }
    let total = 0;
    for (let i = 0; i < 1024; i++) total += values[i];
    return total;
}

export function run(bias) {
    let total = 0;
    for (let i = 0; i < 1024; i++) {
        const lane = i & 3;
        if (lane === 0) total += i - bias;
        else if (lane === 1) total += i * 2;
        else if (lane === 2) total -= i;
        else total += 0 - bias;
    }
    return total;
}

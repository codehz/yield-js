function step(value) {
    return value + 3;
}

export function run() {
    let total = 0;
    for (let i = 0; i < 1024; i++) total += step(i);
    return total;
}

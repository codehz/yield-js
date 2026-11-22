export function run() {
    const values = [];
    for (let i = 0; i < 1024; i++) values.push(i);
    let total = 0;
    for (let i = 0; i < 1024; i++) total += values[i];
    for (let i = 1023; i >= 0; i--) total += values[i];
    for (let i = 0; i < values.length; i++) total += 1;
    return total;
}

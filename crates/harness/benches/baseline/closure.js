export function run() {
    let value = 0;
    const update = (delta) => { value += delta; };
    const read = () => value;
    for (let i = 0; i < 1024; i++) update(i & 3);
    return read();
}

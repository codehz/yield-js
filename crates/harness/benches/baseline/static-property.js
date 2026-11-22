export function run() {
    const state = { total: 0, step: 3 };
    for (let i = 0; i < 1024; i++) state.total += state.step;
    return state.total;
}

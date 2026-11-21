// @returns 110

export function run() {
  let log = 0;
  try {
    try {
      return 1;
    } finally {
      log = log + 10;
    }
  } finally {
    log = log + 100;
    return log;
  }
}

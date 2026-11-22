//! Stable non-cryptographic content digests.
//!
//! `build_id` and host binding fingerprints are equality checks against
//! accidental corruption and contract drift. They are not authentication:
//! anyone able to modify the input can recompute the digest. xxh3-128 is chosen
//! for speed and cross-process determinism, not for adversarial collision
//! resistance.

use xxhash_rust::xxh3::Xxh3;

pub(crate) struct Digest(Xxh3);

impl Digest {
    pub(crate) fn new() -> Self {
        Self(Xxh3::new())
    }

    pub(crate) fn update(&mut self, bytes: impl AsRef<[u8]>) {
        self.0.update(bytes.as_ref());
    }

    pub(crate) fn finish(self) -> String {
        format!("xxh3-128:{:032x}", self.0.digest128())
    }
}

impl std::io::Write for Digest {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

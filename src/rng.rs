use esp_hal::rng::Rng;
use rand_core::{CryptoRng, RngCore};

#[derive(Clone)]
pub struct RngWrapper(Rng);

impl From<Rng> for RngWrapper {
    fn from(rng: Rng) -> Self {
        Self(rng)
    }
}

impl RngCore for RngWrapper {
    fn next_u32(&mut self) -> u32 {
        self.0.random()
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0_u8; u64::BITS as usize / 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut chunks = dest.chunks_exact_mut(4);
        while let Some(chunk) = chunks.next() {
            chunk.copy_from_slice(&self.next_u32().to_le_bytes());
        }

        let remainder = chunks.into_remainder();
        if remainder.len() > 0 {
            remainder.copy_from_slice(&self.next_u32().to_le_bytes()[..remainder.len()]);
        }
    }
}

impl CryptoRng for RngWrapper {}

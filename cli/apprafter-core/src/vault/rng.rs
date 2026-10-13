// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Where the vault's random bytes come from: the operating system, and nothing else (overview
//! §3.7). Ids, salts, nonces, the data key and a recovery key are drawn through [`VaultRng`],
//! so a test can script every byte a vault is made of and reproduce a frozen vector.

use zeroize::Zeroize;

use super::error::{VaultError, VaultResult};

/// A source of the vault's random bytes.
pub trait VaultRng {
    /// Fills `buf`, or fails with `RandomUnavailable` and leaves it zeroed.
    fn fill(&mut self, buf: &mut [u8]) -> VaultResult<()>;
}

/// The operating system's generator (`getrandom`). A failure is `RandomUnavailable`: nothing
/// is written and there is no fallback generator.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsRandom;

impl VaultRng for OsRandom {
    fn fill(&mut self, buf: &mut [u8]) -> VaultResult<()> {
        getrandom::fill(buf).map_err(|e| {
            buf.zeroize();
            VaultError::RandomUnavailable {
                detail: e.to_string(),
            }
        })
    }
}

/// Tests, here and in other crates (`test-support`): yields recorded chunks in order, each
/// exactly the size asked for. It panics when asked for more than it holds or for another
/// size, and when dropped with chunks left, so a vector whose draws moved cannot pass silently.
#[cfg(any(test, feature = "test-support"))]
pub struct ScriptedRandom {
    chunks: std::collections::VecDeque<Vec<u8>>,
    fail_at: Option<usize>,
    drawn: usize,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedRandom {
    pub fn new(chunks: Vec<Vec<u8>>) -> Self {
        ScriptedRandom {
            chunks: chunks.into(),
            fail_at: None,
            drawn: 0,
        }
    }

    /// Yields `chunks` for the draws before `draw` (0-based), then fails that draw with
    /// `RandomUnavailable`.
    pub fn failing_at(draw: usize, chunks: Vec<Vec<u8>>) -> Self {
        assert_eq!(
            chunks.len(),
            draw,
            "one chunk per draw before the failing one"
        );
        ScriptedRandom {
            chunks: chunks.into(),
            fail_at: Some(draw),
            drawn: 0,
        }
    }

    /// How many draws were made, the failing one included.
    pub fn drawn(&self) -> usize {
        self.drawn
    }
}

#[cfg(any(test, feature = "test-support"))]
impl VaultRng for ScriptedRandom {
    fn fill(&mut self, buf: &mut [u8]) -> VaultResult<()> {
        let draw = self.drawn;
        self.drawn += 1;
        if self.fail_at == Some(draw) {
            buf.zeroize();
            return Err(VaultError::RandomUnavailable {
                detail: format!("scripted failure of draw {draw}"),
            });
        }
        let chunk = self.chunks.pop_front().unwrap_or_else(|| {
            panic!(
                "ScriptedRandom: draw {draw} asks for {} bytes, and the script is used up",
                buf.len()
            )
        });
        assert_eq!(
            chunk.len(),
            buf.len(),
            "ScriptedRandom: draw {draw} asks for {} bytes, the script holds {}",
            buf.len(),
            chunk.len()
        );
        buf.copy_from_slice(&chunk);
        Ok(())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for ScriptedRandom {
    fn drop(&mut self) {
        if !std::thread::panicking() && !self.chunks.is_empty() {
            panic!(
                "ScriptedRandom: dropped with {} chunks left after {} draws",
                self.chunks.len(),
                self.drawn
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_os_generator_fills_every_draw_anew() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        OsRandom.fill(&mut a).unwrap();
        OsRandom.fill(&mut b).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
    }

    #[test]
    fn a_script_yields_its_chunks_in_order() {
        let mut rng = ScriptedRandom::new(vec![vec![1, 2], vec![3]]);
        let (mut a, mut b) = ([0u8; 2], [0u8; 1]);
        rng.fill(&mut a).unwrap();
        rng.fill(&mut b).unwrap();
        assert_eq!((a, b, rng.drawn()), ([1, 2], [3], 2));
    }

    #[test]
    #[should_panic(expected = "the script is used up")]
    fn a_script_refuses_an_extra_draw() {
        let mut rng = ScriptedRandom::new(vec![]);
        let _ = rng.fill(&mut [0u8; 4]);
    }

    #[test]
    #[should_panic(expected = "asks for 2 bytes, the script holds 3")]
    fn a_script_refuses_a_draw_of_another_size() {
        let mut rng = ScriptedRandom::new(vec![vec![0; 3]]);
        let _ = rng.fill(&mut [0u8; 2]);
    }

    #[test]
    #[should_panic(expected = "dropped with 1 chunks left")]
    fn a_script_left_unused_fails_its_test() {
        drop(ScriptedRandom::new(vec![vec![0; 3]]));
    }

    #[test]
    fn a_script_fails_the_draw_it_names_and_zeroes_its_buffer() {
        let mut rng = ScriptedRandom::failing_at(1, vec![vec![7]]);
        let mut first = [0u8; 1];
        rng.fill(&mut first).unwrap();
        assert_eq!(first, [7]);
        let mut second = [9u8; 4];
        assert!(matches!(
            rng.fill(&mut second),
            Err(VaultError::RandomUnavailable { .. })
        ));
        assert_eq!((second, rng.drawn()), ([0; 4], 2));
    }
}

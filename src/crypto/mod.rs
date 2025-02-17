//! Cryptographic algorithms and tools.

pub mod chacha20;
pub mod checksum;
pub mod rand;

use crate::errno::EResult;

/// Initializes cryptographic features.
pub fn init() -> EResult<()> {
	rand::init()
}

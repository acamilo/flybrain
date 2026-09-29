//! The boundary memory image and the cartridge beside it (MEM-01).
//!
//! `docs/design/session-framework/legacy-gameboy-v1.md` section 8: on the session framework the
//! macros run in the coordinator's action executor, not beside a live emulator. What they read
//! there is one 64 KiB image of the CPU address space per boundary, carried as an artifact in the
//! observation's `inspection`, plus the ROM as an `AssetRef`. This module is the read side of that
//! seam and nothing else:
//!
//! - [`MemoryImage`] is the image: for every memory address ([`CAPTURED`]) byte *i* is what
//!   `fly_gb_read_mem(i)` returned at the boundary, taken in one bulk read by
//!   [`crate::Emulator::read_memory_image`]; the register windows read [`NOT_CAPTURED`].
//! - [`Cartridge`] is the ROM, content-addressed by its SHA-256, answering the bank-addressed
//!   reads the image cannot ([`MemoryReader::read_rom`]).
//! - [`ImageReader`] puts the two behind [`MemoryReader`], so the **same** macro engine, scene
//!   detector and reward adapter that read a live [`crate::Emulator`] read an image instead. There
//!   is no second implementation of any rule: [`crate::pokemon_red::state::PokeState`] takes a
//!   `&mut dyn MemoryReader`, and this is one.
//!
//! Nothing here writes to a game. An image is a copy; reading it cannot move an emulator.

use std::fmt;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::adapter::MemoryReader;

/// Bytes in one image: `$0000..=$FFFF`. `fly-session-types`' `gameboy::MEMORY_IMAGE_BYTES`.
pub const MEMORY_IMAGE_LEN: usize = 0x1_0000;

/// The artifact's content type (`legacy-gameboy-v1` section 8: "a listed bus attachment, content
/// type `application/octet-stream`"). The encoding is the raw bytes in address order, nothing
/// before and nothing after, so the artifact's `byteLength` is always [`MEMORY_IMAGE_LEN`].
pub const MEMORY_IMAGE_CONTENT_TYPE: &str = "application/octet-stream";

/// What an address outside [`CAPTURED`] holds in an image: `$FF`, which is also what binjgb
/// returns for a read the hardware refuses (`INVALID_READ_BYTE`).
pub const NOT_CAPTURED: u8 = 0xff;

/// The address ranges an image captures, inclusive: every *memory* byte of the address space.
///
/// Each one is exactly what `fly_gb_read_mem` returned at the boundary: ROM as mapped, cartridge
/// RAM, work RAM and its echo, the unused block, high RAM and `IE`. The three *register* windows
/// between them -- VRAM `$8000-$9FFF`, OAM `$FE00-$FE9F`, I/O with the APU and wave RAM
/// `$FF00-$FF7F` -- are not captured and read [`NOT_CAPTURED`]: binjgb's read of a register
/// first runs the subsystem's lazy catch-up (`ppu_synchronize`, `timer_synchronize`, ...), which
/// rewrites its sync bookkeeping, so reading one moves the exported state (`legacy-gameboy-v1`
/// section 8, amendment of 2026-09-29). Nothing the tasks or executors read is in them.
pub const CAPTURED: [(u16, u16); 4] =
    [(0x0000, 0x7fff), (0xa000, 0xfdff), (0xfea0, 0xfeff), (0xff80, 0xffff)];

/// Whether an image captures `address` ([`CAPTURED`]).
pub fn captured(address: u16) -> bool {
    CAPTURED.iter().any(|&(low, high)| (low..=high).contains(&address))
}

/// Bytes in one ROM bank (`$4000`).
const ROM_BANK_BYTES: usize = 0x4000;

/// Why an image or a cartridge was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// An image is exactly [`MEMORY_IMAGE_LEN`] bytes.
    ImageSize { actual: usize },
    /// The cartridge's SHA-256 is not the one the composition names.
    RomDigest { expected: String, actual: String },
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImageSize { actual } => {
                write!(f, "a memory image is {MEMORY_IMAGE_LEN} bytes, got {actual}")
            }
            Self::RomDigest { expected, actual } => {
                write!(f, "the cartridge's sha256 is {actual}, the composition names {expected}")
            }
        }
    }
}

impl std::error::Error for ImageError {}

/// One boundary's view of the CPU address space.
///
/// Owned and immutable once taken: it is the payload of an inspection artifact, which the
/// coordinator keeps until the transition that reads it has been evaluated
/// (`state-media-v1` section 3, RT-01a amendment).
#[derive(Clone, PartialEq, Eq)]
pub struct MemoryImage {
    bytes: Box<[u8; MEMORY_IMAGE_LEN]>,
}

impl MemoryImage {
    /// An image of zeroes, to be filled by [`crate::Emulator::read_memory_image_into`].
    pub fn zeroed() -> Self {
        let bytes: Box<[u8]> = vec![0u8; MEMORY_IMAGE_LEN].into_boxed_slice();
        Self { bytes: bytes.try_into().expect("exactly MEMORY_IMAGE_LEN bytes") }
    }

    /// Decode an artifact's bytes. Exactly [`MEMORY_IMAGE_LEN`] of them, or refused.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ImageError> {
        if bytes.len() != MEMORY_IMAGE_LEN {
            return Err(ImageError::ImageSize { actual: bytes.len() });
        }
        let mut image = Self::zeroed();
        image.bytes.copy_from_slice(bytes);
        Ok(image)
    }

    /// The artifact's bytes: the address space in address order.
    pub fn as_bytes(&self) -> &[u8; MEMORY_IMAGE_LEN] {
        &self.bytes
    }

    pub(crate) fn as_mut_bytes(&mut self) -> &mut [u8; MEMORY_IMAGE_LEN] {
        &mut self.bytes
    }

    /// The byte at a CPU address.
    pub fn byte(&self, address: u16) -> u8 {
        self.bytes[usize::from(address)]
    }

    /// SHA-256 of the artifact's bytes, 64 lowercase hex digits: the bus `Digest` encoding.
    ///
    /// Optional on the live artifact (a transient frame, `state-media-v1` section 1); mandatory
    /// only if an image is ever carried in a checkpoint payload.
    pub fn sha256_hex(&self) -> String {
        hex(&Sha256::digest(self.bytes.as_slice()))
    }
}

impl fmt::Debug for MemoryImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryImage").field("sha256", &self.sha256_hex()).finish()
    }
}

/// The ROM, as the executor holds it: the composition's `AssetRef`, content-addressed.
///
/// Cheap to clone (one `Arc`). The emulator holds one too, for its own [`MemoryReader::read_rom`],
/// so the live reader and the image reader answer a bank read from the same function over the
/// same bytes.
#[derive(Clone)]
pub struct Cartridge {
    bytes: Arc<[u8]>,
    sha256: [u8; 32],
}

impl Cartridge {
    /// The cartridge as supplied: the bytes the emulator was given, before its padding.
    pub fn new(rom: &[u8]) -> Self {
        Self { bytes: rom.into(), sha256: Sha256::digest(rom).into() }
    }

    /// The cartridge only if its SHA-256 is `expected` (lowercase hex), which is how the executor
    /// refuses a ROM that is not the environment's `contentDigest` (`legacy-gameboy-v1` section 8).
    pub fn verified(rom: &[u8], expected: &str) -> Result<Self, ImageError> {
        let cartridge = Self::new(rom);
        let actual = cartridge.sha256_hex();
        if actual != expected {
            return Err(ImageError::RomDigest { expected: expected.to_string(), actual });
        }
        Ok(cartridge)
    }

    /// SHA-256 of the ROM bytes as supplied, lowercase hex: the compatibility string's `romSha256`,
    /// the environment's `contentDigest` and the inspection's `romDigest`.
    pub fn sha256_hex(&self) -> String {
        hex(&self.sha256)
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// One byte of ROM bank `bank` at CPU address `address`: see [`MemoryReader::read_rom`].
    pub fn read_bank(&self, bank: u8, address: u16) -> Option<u8> {
        let offset = match address {
            0x0000..=0x3fff => usize::from(address),
            0x4000..=0x7fff => {
                usize::from(bank) * ROM_BANK_BYTES + usize::from(address) - ROM_BANK_BYTES
            }
            _ => return None,
        };
        self.bytes.get(offset).copied()
    }
}

impl fmt::Debug for Cartridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cartridge")
            .field("sha256", &self.sha256_hex())
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// A [`MemoryReader`] over one boundary's image and the cartridge.
///
/// What the executor hands the macro engine in Phase B, and the task in Phase C: the same
/// `&mut dyn MemoryReader` the legacy loop hands them with a live emulator behind it. It needs no
/// cache of its own, because an image is already one frame's reads.
#[derive(Debug, Clone, Copy)]
pub struct ImageReader<'a> {
    image: &'a MemoryImage,
    rom: &'a Cartridge,
}

impl<'a> ImageReader<'a> {
    pub fn new(image: &'a MemoryImage, rom: &'a Cartridge) -> Self {
        Self { image, rom }
    }

    pub fn image(&self) -> &'a MemoryImage {
        self.image
    }
}

impl MemoryReader for ImageReader<'_> {
    fn read8(&mut self, address: u16) -> u8 {
        self.image.byte(address)
    }

    fn read_rom(&mut self, bank: u8, address: u16) -> Option<u8> {
        self.rom.read_bank(bank, address)
    }
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_is_exactly_the_address_space() {
        assert_eq!(MEMORY_IMAGE_LEN, usize::from(u16::MAX) + 1);
        assert_eq!(
            MemoryImage::from_bytes(&[0; MEMORY_IMAGE_LEN - 1]).unwrap_err(),
            ImageError::ImageSize { actual: MEMORY_IMAGE_LEN - 1 }
        );
        assert_eq!(
            MemoryImage::from_bytes(&[0; MEMORY_IMAGE_LEN + 1]).unwrap_err(),
            ImageError::ImageSize { actual: MEMORY_IMAGE_LEN + 1 }
        );
    }

    #[test]
    fn the_encoding_is_the_bytes_in_address_order() {
        let bytes: Vec<u8> = (0..MEMORY_IMAGE_LEN).map(|i| (i ^ (i >> 8)) as u8).collect();
        let image = MemoryImage::from_bytes(&bytes).unwrap();
        assert_eq!(image.as_bytes().as_slice(), bytes.as_slice());
        let rom = Cartridge::new(&[0; 0x8000]);
        let mut reader = ImageReader::new(&image, &rom);
        for address in [0x0000u16, 0x00ff, 0xc000, 0xd35e, 0xff80, 0xffff] {
            assert_eq!(reader.read8(address), bytes[usize::from(address)], "{address:#06x}");
        }
        // The digest is over exactly those bytes, in the bus `Digest` encoding.
        assert_eq!(image.sha256_hex(), hex(&Sha256::digest(&bytes)));
        assert_eq!(image.sha256_hex().len(), 64);
        assert!(image.sha256_hex().bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    }

    #[test]
    fn the_captured_ranges_are_the_memory_between_the_register_windows() {
        let captured_count = (0..=u16::MAX).filter(|&address| captured(address)).count();
        // 64 KiB less VRAM (8 KiB), OAM (160 B) and I/O (128 B).
        assert_eq!(captured_count, MEMORY_IMAGE_LEN - 0x2000 - 0xa0 - 0x80);
        for register in [0x8000u16, 0x9fff, 0xfe00, 0xfe9f, 0xff00, 0xff44, 0xff7f] {
            assert!(!captured(register), "{register:#06x}");
        }
        for memory in [0x0000u16, 0x7fff, 0xa000, 0xc000, 0xdfff, 0xe000, 0xfdff, 0xfea0, 0xff80, 0xffff] {
            assert!(captured(memory), "{memory:#06x}");
        }
        // Every symbol the Pokémon macros and adapter name is in WRAM or HRAM.
        use crate::pokemon_red::symbols::ram;
        for symbol in [ram::wCurMap, ram::wPartyCount, ram::wTileMap, ram::wStatusFlags7, ram::wListMenuID] {
            assert!(captured(symbol), "{symbol:#06x}");
        }
    }

    #[test]
    fn a_bank_read_is_the_emulators_rule() {
        // Four banks, each byte naming its bank.
        let rom: Vec<u8> = (0..4 * ROM_BANK_BYTES).map(|i| (i / ROM_BANK_BYTES) as u8).collect();
        let cartridge = Cartridge::new(&rom);
        // Bank 0 is always mapped, whatever the bank asked for.
        assert_eq!(cartridge.read_bank(3, 0x0100), Some(0));
        assert_eq!(cartridge.read_bank(3, 0x4000), Some(3));
        assert_eq!(cartridge.read_bank(1, 0x7fff), Some(1));
        // A bank the cartridge does not have, and an address that is not ROM.
        assert_eq!(cartridge.read_bank(4, 0x4000), None);
        assert_eq!(cartridge.read_bank(1, 0x8000), None);
        assert_eq!(cartridge.read_bank(1, 0xc000), None);
    }

    #[test]
    fn a_cartridge_is_refused_under_another_digest() {
        let rom = [7u8; 0x8000];
        let digest = Cartridge::new(&rom).sha256_hex();
        assert!(Cartridge::verified(&rom, &digest).is_ok());
        let wrong = "0".repeat(64);
        assert!(matches!(
            Cartridge::verified(&rom, &wrong),
            Err(ImageError::RomDigest { .. })
        ));
    }
}

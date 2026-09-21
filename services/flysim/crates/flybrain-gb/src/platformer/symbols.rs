//! Super Mario Land RAM addresses, with a source for every one.
//!
//! Taken from `docs/design/platformer.md` §1, which took them from Kasper
//! Meerts' disassembly (<https://github.com/kaspermeerts/supermarioland>):
//! `hram.asm` and `wram.asm` name the symbols, and the cited `bank0.asm` lines
//! are the code that gives each byte its meaning.
//!
//! Two classes of address live here:
//!
//! - **named**, appearing as a label in `hram.asm` or `wram.asm`. Trusted.
//! - **UNVERIFIED**, appearing in the disassembly only as a comment on a bare
//!   literal. Every one carries an `UNVERIFIED` marker and is listed in
//!   [`UNVERIFIED`] so a bring-up spike and the honesty panel can enumerate
//!   them. `docs/design/platformer.md` §8.1 asks for a bgb/mGBA watchpoint pass
//!   on each before this adapter ships against a real cartridge.
//!
//! Only WRAM (`0xC000..=0xDFFF`) and HRAM (`0xFF80..=0xFFFE`) are read. VRAM is
//! deliberately never read: binjgb's public read path returns `0xFF` for VRAM
//! during PPU mode 3 (`docs/design/platformer.md` §1, "Is reading VRAM
//! acceptable?"), so a HUD read would be silently wrong one frame in three.
#![allow(dead_code, non_upper_case_globals)]

/// Disassembly this map was read from.
pub const SML_DISASSEMBLY: &str = "kaspermeerts/supermarioland";

/// SHA-1 the disassembly builds to, i.e. the revision these addresses describe:
/// Super Mario Land (World) (Rev A). Recorded for provenance only — the adapter
/// pins a SHA-256 supplied by configuration, because the Rev A SHA-256 is not
/// known here (`docs/design/platformer.md` §8.5).
pub const SML_ROM_SHA1: &str = "418203621b887caa090215d97e3f509b79affd3e";

/// HRAM, from `hram.asm` unless marked otherwise.
pub mod hram {
    /// 0 while the game runs, nonzero while paused. hram.asm; bank0.asm:1083-1090.
    pub const hGamePaused: u16 = 0xffb2;
    /// Game-state jump table index. hram.asm; table listed at bank0.asm:2A6.
    /// `0x00` normal gameplay, `0x01` dead, `0x02` reset to checkpoint,
    /// `0x03` pre-dying, `0x04` dying, `0x05` explosion / score countdown,
    /// `0x06` end of level, `0x07` end-of-level gate, `0x08` increment level,
    /// `0x09`-`0x0C` pipe transitions, `0x0D` autoscrolling level,
    /// `0x0E` init menu, `0x0F` start menu, `0x11` level start, `0x12` bonus
    /// game, `0x39` game-over text, `0x3A` game-over wait
    /// (bank0.asm:4291-4348, 2784-2786).
    pub const hGameState: u16 = 0xffb3;
    /// World and level as BCD nibbles: 1-1 is `0x11`. hram.asm; encoding at
    /// bank0.asm:711, 746.
    pub const hWorldAndLevel: u16 = 0xffb4;
    /// Nonzero while Superball Mario. hram.asm.
    pub const hSuperballMario: u16 = 0xffb5;
    /// Written straight to `rSCX`. hram.asm; bank0.asm:2775-2776.
    pub const hScrollX: u16 = 0xffa4;
    /// 0 small, 1 growing, 2 super, 3+ injury i-frames. hram.asm;
    /// bank0.asm:1116, 1380-1385, 1408 (`InjureMario`).
    pub const hSuperStatus: u16 = 0xff99;
    /// Game clears. hram.asm; incremented at bank0.asm:3141-3144 after
    /// "THE END". Mirrored by [`super::wram::wWinCount`].
    pub const hWinCount: u16 = 0xff9a;
    /// Stomp-chain timer, 50 frames. hram.asm; bank0.asm:1285-1301.
    pub const hStompChainTimer: u16 = 0xff9c;
    /// Stomp chain, capped at 3. hram.asm; bank0.asm:1285-1301.
    pub const hStompChain: u16 = 0xff9d;
    /// Level index, 0..11. hram.asm; `cp a, $0C ; 12 levels in total`
    /// bank0.asm:700-702.
    pub const hLevelIndex: u16 = 0xffe4;
    /// Column-loader screen index, incremented every 20 columns. hram.asm;
    /// bank0.asm:5215-5223. Levels start at 3 (bank0.asm:2071).
    pub const hScreenIndex: u16 = 0xffe5;
    /// Column within the screen, 0..19. hram.asm; `cp a, $14 ; 20 columns per
    /// screen?` bank0.asm:5217.
    pub const hColumnIndex: u16 = 0xffe6;
    /// Coins, BCD, 0..99. hram.asm.
    pub const hCoins: u16 = 0xfffa;

    /// **UNVERIFIED.** Attract-demo gate. Unnamed in hram.asm; bank0.asm:341
    /// comments it "Equal to 28 in menu and during demo", and `Call_2113`
    /// (bank0.asm:5060-5066) overwrites `hJoyHeld` from `0xC0DB` only while it
    /// is nonzero. The most load-bearing unverified byte in this map: without it
    /// the attract demo farms rewards.
    pub const UNNAMED_DEMO_GATE: u16 = 0xff9f;
    /// **UNVERIFIED.** Nonzero underground (pipe sub-room). Comment-only.
    /// Suppresses `band` payouts and snapshots, because pipe sub-rooms reuse
    /// `hScreenIndex` and would alias onto the main level's bands
    /// (`docs/design/platformer.md` §8.3).
    pub const UNNAMED_UNDERGROUND: u16 = 0xfff9;
    /// **UNVERIFIED.** Pipe-exit pair, comment-only. Unused by this adapter;
    /// recorded because §8.3 revisits underground banding once they are
    /// confirmed.
    pub const UNNAMED_PIPE_EXIT_LO: u16 = 0xfff4;
    /// **UNVERIFIED.** See [`UNNAMED_PIPE_EXIT_LO`].
    pub const UNNAMED_PIPE_EXIT_HI: u16 = 0xfff5;
}

/// WRAM, from `wram.asm` unless marked otherwise.
pub mod wram {
    /// Score, 3 bytes BCD, most significant byte first. wram.asm.
    pub const wScore: u16 = 0xc0a0;
    /// `+1` on a 1UP (bank0.asm:1399), `0xFF` on death (bank0.asm:925). wram.asm.
    pub const wLivesEarnedLost: u16 = 0xc0a3;
    /// Nonzero while the game-over window is up. wram.asm.
    pub const wGameOverWindowEnabled: u16 = 0xc0a5;
    /// Nonzero once the game-over timer has expired. wram.asm.
    pub const wGameOverTimerExpired: u16 = 0xc0ad;
    /// Invincibility (star) timer. wram.asm.
    pub const wInvincibilityTimer: u16 = 0xc0d3;
    /// Mirror of [`super::hram::hWinCount`]. wram.asm.
    pub const wWinCount: u16 = 0xc0e1;
    /// Game timer, 3 bytes BCD. wram.asm.
    pub const wGameTimer: u16 = 0xda00;
    /// Remaining lives. wram.asm.
    pub const wLives: u16 = 0xda15;
    /// Nonzero once the game timer is running out. wram.asm.
    pub const wGameTimerExpiringFlag: u16 = 0xda1d;

    /// **UNVERIFIED.** Jump status: 0 on ground, 1 ascending, 2 descending.
    /// Comment-only, bank0.asm:4409. Part of the safe-snapshot gate.
    pub const UNNAMED_JUMP_STATUS: u16 = 0xc207;
    /// **UNVERIFIED.** 1 while on the ground. Comment-only, bank0.asm:1258
    /// (`ld hl, $C20A ; 1 if on ground`). Part of the safe-snapshot gate.
    pub const UNNAMED_ON_GROUND: u16 = 0xc20a;
    /// **UNVERIFIED.** Running momentum, 0..6. Comment-only,
    /// bank0.asm:4418-4429. Unused; recorded because it is the reason the
    /// platformer decoder preset holds a direction gaplessly.
    pub const UNNAMED_MOMENTUM: u16 = 0xc20c;
    /// **UNVERIFIED.** Set to `0x02` once momentum saturates. Comment-only,
    /// bank0.asm:4418-4429. Unused, same reason as [`UNNAMED_MOMENTUM`].
    pub const UNNAMED_RUNNING: u16 = 0xc20e;
    /// **UNVERIFIED.** "sort of progress in the level in columns / 2",
    /// bank0.asm:3361. Unused: the two named HRAM bytes are preferred.
    pub const UNNAMED_COLUMN_PROGRESS: u16 = 0xc0ab;
    /// **UNVERIFIED.** End-of-level counter, comment-only. Unused.
    pub const UNNAMED_END_OF_LEVEL: u16 = 0xc0d2;
}

/// Every address this map could not verify against a disassembly label, as
/// `(address, what it is believed to be)`. Ordered by address, so the list reads
/// the same every time it is printed.
pub const UNVERIFIED: [(u16, &str); 10] = [
    (wram::UNNAMED_COLUMN_PROGRESS, "column progress (unused)"),
    (wram::UNNAMED_END_OF_LEVEL, "end-of-level counter (unused)"),
    (wram::UNNAMED_JUMP_STATUS, "jump status"),
    (wram::UNNAMED_ON_GROUND, "on the ground"),
    (wram::UNNAMED_MOMENTUM, "running momentum (unused)"),
    (wram::UNNAMED_RUNNING, "running flag (unused)"),
    (hram::UNNAMED_DEMO_GATE, "attract-demo gate"),
    (hram::UNNAMED_PIPE_EXIT_LO, "pipe exit low (unused)"),
    (hram::UNNAMED_PIPE_EXIT_HI, "pipe exit high (unused)"),
    (hram::UNNAMED_UNDERGROUND, "underground"),
];

/// Addresses the adapter actually reads and whose meaning is unverified. A
/// subset of [`UNVERIFIED`]; the rest are recorded for the bring-up spike only.
pub const UNVERIFIED_AND_USED: [u16; 4] = [
    hram::UNNAMED_DEMO_GATE,
    hram::UNNAMED_UNDERGROUND,
    wram::UNNAMED_JUMP_STATUS,
    wram::UNNAMED_ON_GROUND,
];

/// Screens per level, from `levels/levels.asm`: one screen pointer per screen,
/// terminated by `db $ff`. Indexed by `hLevelIndex` (0 = 1-1 .. 11 = 4-3).
pub const LEVEL_SCREENS: [u32; 12] = [18, 17, 18, 19, 17, 21, 26, 19, 18, 26, 23, 27];

/// Levels in the game; `hLevelIndex < 12` (`cp a, $0C`, bank0.asm:700-702).
pub const LEVEL_COUNT: u32 = 12;

/// Every level starts at this screen (`ld a, $03 / ldh [hScreenIndex], a ; do
/// all levels start on screen 3`, bank0.asm:2071).
pub const FIRST_SCREEN: u32 = 3;

/// Columns per screen (`cp a, $14`, bank0.asm:5217).
pub const COLUMNS_PER_SCREEN: u32 = 20;

/// Column index a level starts at: [`FIRST_SCREEN`] * [`COLUMNS_PER_SCREEN`].
pub const FIRST_COLUMN: u32 = FIRST_SCREEN * COLUMNS_PER_SCREEN;

/// Columns per reward band. Ten columns is half a screen.
pub const BAND_COLUMNS: u32 = 10;

/// Checkpoint screens a death can restore to, from `GameState_02`
/// (bank0.asm:952-978); the matching `0xC0AB` values are `0C, 34, 5C, 84, AC,
/// D4`. Display only — the adapter does not depend on them.
pub const CHECKPOINT_SCREENS: [u32; 6] = [3, 7, 11, 15, 19, 23];

/// Normal, controllable gameplay.
pub const GAME_STATE_NORMAL: u8 = 0x00;
/// Autoscrolling (vehicle) level. Playable, never safe to snapshot.
pub const GAME_STATE_AUTOSCROLL: u8 = 0x0d;
/// Prepares the game-over text (bank0.asm:4291-4348).
pub const GAME_STATE_GAME_OVER_TEXT: u8 = 0x39;
/// Game-over wait (`cp a, $3A / jr nz, .out ; game not over`,
/// bank0.asm:2784-2786).
pub const GAME_STATE_GAME_OVER_WAIT: u8 = 0x3a;

/// Screens in `level`, or `None` for an out-of-range index.
pub fn screens(level: u32) -> Option<u32> {
    LEVEL_SCREENS.get(level as usize).copied()
}

/// Columns of playable ground in `level`: `(screens - 3) * 20`.
pub fn level_columns(level: u32) -> Option<u32> {
    screens(level).map(|screens| (screens - FIRST_SCREEN) * COLUMNS_PER_SCREEN)
}

/// Reward bands in `level`: `2 * (screens - 3)`, the cap in
/// `docs/design/platformer.md` §2 (30 for 1-1, 48 for 4-3).
pub fn level_bands(level: u32) -> Option<u32> {
    level_columns(level).map(|columns| columns / BAND_COLUMNS)
}

/// Decode a big-endian BCD field of one to three bytes. `None` when any nibble
/// is above 9, which is how a mid-write or a wrong-revision read shows up.
pub fn bcd(bytes: &[u8]) -> Option<u32> {
    let mut value = 0u32;
    for byte in bytes {
        let (high, low) = (byte >> 4, byte & 0x0f);
        if high > 9 || low > 9 {
            return None;
        }
        value = value * 100 + u32::from(high) * 10 + u32::from(low);
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_table_matches_the_design_document() {
        assert_eq!(LEVEL_SCREENS.len(), LEVEL_COUNT as usize);
        // 1-1 and 4-3, the two band caps the design names explicitly.
        assert_eq!(level_bands(0), Some(30));
        assert_eq!(level_bands(11), Some(48));
        assert_eq!(level_columns(0), Some(300));
        assert_eq!(screens(12), None);
        assert!(LEVEL_SCREENS.iter().all(|screens| *screens > FIRST_SCREEN));
    }

    #[test]
    fn bcd_rejects_a_nibble_above_nine() {
        assert_eq!(bcd(&[0x99]), Some(99));
        assert_eq!(bcd(&[0x00]), Some(0));
        assert_eq!(bcd(&[0x01, 0x23, 0x45]), Some(12345));
        assert_eq!(bcd(&[0x0a]), None);
        assert_eq!(bcd(&[0xf0]), None);
    }

    #[test]
    fn every_address_is_wram_or_hram_and_never_vram() {
        let addresses: Vec<u16> = UNVERIFIED
            .iter()
            .map(|(address, _)| *address)
            .chain([
                hram::hGamePaused,
                hram::hGameState,
                hram::hWorldAndLevel,
                hram::hSuperballMario,
                hram::hScrollX,
                hram::hSuperStatus,
                hram::hWinCount,
                hram::hStompChain,
                hram::hStompChainTimer,
                hram::hLevelIndex,
                hram::hScreenIndex,
                hram::hColumnIndex,
                hram::hCoins,
                wram::wScore,
                wram::wLivesEarnedLost,
                wram::wGameOverWindowEnabled,
                wram::wGameOverTimerExpired,
                wram::wInvincibilityTimer,
                wram::wWinCount,
                wram::wGameTimer,
                wram::wLives,
                wram::wGameTimerExpiringFlag,
            ])
            .collect();
        for address in addresses {
            let wram = (0xc000..=0xdfff).contains(&address);
            let hram = (0xff80..=0xfffe).contains(&address);
            assert!(wram || hram, "{address:#06x} is neither WRAM nor HRAM");
        }
    }

    #[test]
    fn the_unverified_list_is_sorted_and_covers_the_used_subset() {
        assert!(UNVERIFIED.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for address in UNVERIFIED_AND_USED {
            assert!(
                UNVERIFIED.iter().any(|(listed, _)| *listed == address),
                "{address:#06x} is used but not listed as unverified"
            );
        }
    }
}

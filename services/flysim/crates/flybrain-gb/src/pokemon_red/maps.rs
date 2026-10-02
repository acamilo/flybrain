//! Kanto's map ids (`constants/map_constants.asm` at [`super::symbols::POKERED_COMMIT`]).
//!
//! One module for the ids, because three places need them and a second copy of a number is how a
//! wrong one survives: the ladder's rung conditions and rung places ([`super::RUNGS`],
//! [`super::rung_place`]), the macro layer's geography ([`super::macros::geography`]) and the
//! tests.
//!
//! The decomp numbers the nine towns and cities, Indigo Plateau, Saffron, one unused id and then
//! `ROUTE_1` through `ROUTE_25` as `$00`..`$24`; `REDS_HOUSE_1F` at `$25` begins the interiors and
//! they run in the order the decomp lists them. Every id below is that ordering counted out from
//! an anchor `docs/design/ladder.md` verified against the decomp -- `VIRIDIAN_FOREST` `$33`,
//! `MT_MOON_1F` `$3b`, `ROCK_TUNNEL_1F` `$52`, `INDIGO_PLATEAU_LOBBY` `$ae` -- and
//! [`super::tests`] pins those four so that a mis-counted interior cannot land silently.
//!
//! **2026-09-17, section 13's errands.** Five more interiors, each one counted out *between* two
//! ids this module already pins, which is the only way a mart or a centre id can be wrong without
//! a test noticing: `VIRIDIAN_POKECENTER` is the id immediately before `VIRIDIAN_MART` `$2a`;
//! `PEWTER_MART` `$38` and `PEWTER_POKECENTER` `$3a` sit in the five ids between `PEWTER_GYM`
//! `$36` and `MT_MOON_1F` `$3b`; `CERULEAN_POKECENTER` `$40` and `CERULEAN_MART` `$43` sit around
//! `CERULEAN_GYM` `$41`. [`super::tests`] pins each against its neighbour rather than on its own.
//!
//! **2026-09-22, the rung-10 museum.** `PEWTER_MUSEUM_1F` `$34` and `PEWTER_MUSEUM_2F` `$35` are
//! the two ids between `VIRIDIAN_FOREST` `$33` and `PEWTER_GYM` `$36`, and both are confirmed by
//! the cartridge rather than by counting: Pewter City's warp table names `$34` at (14, 7) and at
//! (19, 5) -- the museum's two doors -- and `$34`'s own warp at (7, 7) names `$35`, which is the
//! staircase. Surveyed from the release container's rung-10 checkpoint
//! (`infra/docs/macros-traps.md`, 2026-09-22).

// Outdoors: `$00`..`$24`.
pub const PALLET_TOWN: u8 = 0x00;
pub const VIRIDIAN_CITY: u8 = 0x01;
pub const PEWTER_CITY: u8 = 0x02;
pub const CERULEAN_CITY: u8 = 0x03;
pub const LAVENDER_TOWN: u8 = 0x04;
pub const VERMILION_CITY: u8 = 0x05;
pub const CELADON_CITY: u8 = 0x06;
pub const FUCHSIA_CITY: u8 = 0x07;
pub const CINNABAR_ISLAND: u8 = 0x08;
pub const INDIGO_PLATEAU: u8 = 0x09;
pub const SAFFRON_CITY: u8 = 0x0a;
pub const ROUTE_1: u8 = 0x0c;
pub const ROUTE_2: u8 = 0x0d;
pub const ROUTE_3: u8 = 0x0e;
pub const ROUTE_4: u8 = 0x0f;
pub const ROUTE_5: u8 = 0x10;
pub const ROUTE_6: u8 = 0x11;
pub const ROUTE_7: u8 = 0x12;
pub const ROUTE_8: u8 = 0x13;
pub const ROUTE_9: u8 = 0x14;
pub const ROUTE_10: u8 = 0x15;
pub const ROUTE_11: u8 = 0x16;
pub const ROUTE_12: u8 = 0x17;
pub const ROUTE_13: u8 = 0x18;
pub const ROUTE_14: u8 = 0x19;
pub const ROUTE_15: u8 = 0x1a;
pub const ROUTE_16: u8 = 0x1b;
pub const ROUTE_17: u8 = 0x1c;
pub const ROUTE_18: u8 = 0x1d;
pub const ROUTE_19: u8 = 0x1e;
pub const ROUTE_20: u8 = 0x1f;
pub const ROUTE_21: u8 = 0x20;
pub const ROUTE_22: u8 = 0x21;
pub const ROUTE_23: u8 = 0x22;
pub const ROUTE_24: u8 = 0x23;
pub const ROUTE_25: u8 = 0x24;

// Interiors: `$25` onward.
pub const REDS_HOUSE_1F: u8 = 0x25;
pub const REDS_HOUSE_2F: u8 = 0x26;
pub const BLUES_HOUSE: u8 = 0x27;
pub const OAKS_LAB: u8 = 0x28;
pub const VIRIDIAN_POKECENTER: u8 = 0x29;
pub const VIRIDIAN_MART: u8 = 0x2a;
pub const VIRIDIAN_GYM: u8 = 0x2d;
pub const VIRIDIAN_FOREST_NORTH_GATE: u8 = 0x2f;
pub const VIRIDIAN_FOREST_SOUTH_GATE: u8 = 0x32;
pub const VIRIDIAN_FOREST: u8 = 0x33;
pub const PEWTER_MUSEUM_1F: u8 = 0x34;
pub const PEWTER_MUSEUM_2F: u8 = 0x35;
pub const PEWTER_GYM: u8 = 0x36;
pub const PEWTER_MART: u8 = 0x38;
pub const PEWTER_POKECENTER: u8 = 0x3a;
pub const MT_MOON_1F: u8 = 0x3b;
pub const MT_MOON_B1F: u8 = 0x3c;
pub const MT_MOON_B2F: u8 = 0x3d;
pub const CERULEAN_POKECENTER: u8 = 0x40;
/// The house the Rockets robbed (row 73): the id before `CERULEAN_TRADE_HOUSE` and
/// `CERULEAN_POKECENTER` `$40`, and confirmed by the cartridge rather than by counting -- Cerulean
/// City's warp table names `$3e` at (27, 11) and at (27, 9), its front door and the hole in its
/// back wall.
pub const CERULEAN_TRASHED_HOUSE: u8 = 0x3e;
pub const CERULEAN_GYM: u8 = 0x41;
pub const CERULEAN_MART: u8 = 0x43;
/// Route 4's Pokémon Center, the one beside Mt. Moon's mouth: the id straight after
/// `CERULEAN_MART` `$43`, and confirmed by the cartridge rather than by counting -- Route 4's warp
/// table names `$44` at (11, 5) (`tests/rom_macros_mode.rs`, row 59's door survey).
pub const MT_MOON_POKECENTER: u8 = 0x44;
/// Bill's house on Route 25, where rung 16 (the S.S. Ticket) is earned (row 73). Confirmed by the
/// cartridge rather than by counting: Route 25's warp table names `$58` at (45, 3), and the
/// cartridge's own hidden-event map list (`HiddenEventMaps`) carries `$58` at the position the
/// disassembly lists `BILLS_HOUSE` in (`tests/rom_macros_mode.rs`, row 73).
pub const BILLS_HOUSE: u8 = 0x58;
pub const ROCK_TUNNEL_1F: u8 = 0x52;
pub const INDIGO_PLATEAU_LOBBY: u8 = 0xae;
/// The house beside Cerulean's west road whose back door is the one way into the closed yard at
/// (9, 9) (row 65). Confirmed by the cartridge rather than by counting: Cerulean City's warp table
/// names `$e6` at (9, 11) and at (9, 9), read from the yard's own survey checkpoint.
pub const CERULEAN_BADGE_HOUSE: u8 = 0xe6;

//! What the sim loop needs from a macro palette, independent of which game it is.
//!
//! `docs/design/macros.md` sections 1 and 4 are the contract. The palette itself is
//! Pokémon-specific and lives in [`crate::pokemon_red::macros`]; this module is the seam the sim
//! loop talks to, so `flysim` never names a scene, a macro or an address — exactly as
//! [`crate::adapter::GameAdapter`] keeps the loop out of the reward rules.
//!
//! ```text
//! decoder channels --> slot 0..5 --> MacroPalette::start
//!                                         |
//!                 per frame: MacroPalette::step --> button mask --> emulator
//! ```
//!
//! Three things are deliberately *not* here:
//!
//! - **no choosing.** Nothing in this module or behind it picks a macro. The slot arrives from the
//!   readout, and a scene that binds nothing simply presses nothing.
//! - **no new button path.** A macro's mask goes to the same button register the decoder's raw
//!   masks go to. There is still no button endpoint on the control API.
//! - **no mode knowledge.** Whether palette mode is on at all is `flysim.toml`'s business
//!   (`[macros] mode`); a palette built here does nothing until the loop steps it.

use crate::adapter::{GameAdapter, MapExit, MapPlace, MapTile, MemoryReader};

/// What a macro palette may ask of the adapter's lifetime state.
///
/// Four questions across [`crate::adapter`]'s seam -- three about where this run has been and one
/// about where the ladder goes next -- and the only thing the macro palette is told about the
/// reward side. It is a separate trait rather than a `&dyn GameAdapter` parameter because that is
/// all a palette may know: not the rank, not the ledger, not the payouts, and nothing it could
/// write to. The last three have defaults that answer "nothing known", so a ledger that carries
/// only the exit half is still a complete implementation.
pub trait RunLedger {
    /// Whether `GO OUT`, `GO WARP` or `GO ROUTE` has already been through this exit.
    fn exit_visited(&self, exit: MapExit) -> bool;

    /// Whether this run has stood on this tile: `GO FRONTIER`'s question.
    fn tile_visited(&self, tile: MapTile) -> bool {
        let _ = tile;
        false
    }

    /// Whether this run has ever been on this map: `GO ROUTE`'s "unvisited interior".
    fn map_visited(&self, map: u8) -> bool {
        let _ = map;
        false
    }

    /// Where the ladder's next unreached rung is: `GO OBJECTIVE`'s question.
    fn objective(&self) -> Option<MapPlace> {
        None
    }
}

/// An adapter's ledger, in [`RunLedger`] shape.
///
/// The sim loop holds the adapter; this is the one line that hands the palette the one question it
/// may ask of it. Generic over `?Sized` so that both a `Box<dyn GameAdapter>`'s target and a
/// concrete adapter go in without a cast.
#[derive(Debug, Clone, Copy)]
pub struct AdapterLedger<'a, A: ?Sized>(pub &'a A);

impl<A: GameAdapter + ?Sized> RunLedger for AdapterLedger<'_, A> {
    fn exit_visited(&self, exit: MapExit) -> bool {
        self.0.exit_visited(exit)
    }

    fn tile_visited(&self, tile: MapTile) -> bool {
        self.0.tile_visited(tile)
    }

    fn map_visited(&self, map: u8) -> bool {
        self.0.map_visited(map)
    }

    fn objective(&self) -> Option<MapPlace> {
        self.0.objective()
    }
}

/// A ledger that has recorded nothing: every exit unvisited, no ground covered, no objective.
///
/// What a fresh run is, and what a caller with no adapter to hand should pass rather than an
/// invented answer.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLedger;

impl RunLedger for NoLedger {
    fn exit_visited(&self, _exit: MapExit) -> bool {
        false
    }
}

/// Slots one palette can bind: **one per macro type**, so a cell never moves
/// (`docs/design/macros.md` section 14).
///
/// Six until then -- section 1's D-pad and A/B -- which section 12 kept as a cap on how many
/// buttons a scene could deal at once even after a macro stopped being a meaning laid over a
/// button. The cap is what left `MENU` on no pad at all and what the battle's own turn could not
/// fit section 14's four move buttons inside. A slot is now a type index and nothing truncates.
///
/// A game with no palette binds none of them, exactly as before.
pub const SLOTS: u8 = 31;

/// Which scene the game is in, as the feed publishes it (`docs/feed-protocol.md`, `game.scene`).
///
/// A closed set, like `game.mode`: every consumer switches exhaustively on it. A game's own
/// richer scene enum folds onto these names — Pokémon Red's `Battle { own_turn, forced_switch }`
/// becomes [`SceneId::BattleSwitch`] for a forced switch and [`SceneId::Battle`] otherwise,
/// because a forced switch is the one battle state with a different palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SceneId {
    Title,
    Overworld,
    Dialog,
    Menu,
    Battle,
    BattleSwitch,
    Shop,
    Pc,
    Unknown,
}

impl SceneId {
    /// Every scene name, in the order `docs/feed-protocol.md` lists them.
    pub const ALL: [Self; 9] = [
        Self::Title,
        Self::Overworld,
        Self::Dialog,
        Self::Menu,
        Self::Battle,
        Self::BattleSwitch,
        Self::Shop,
        Self::Pc,
        Self::Unknown,
    ];

    /// The lower-case name the feed and the stage use.
    pub const fn feed_name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Overworld => "overworld",
            Self::Dialog => "dialog",
            Self::Menu => "menu",
            Self::Battle => "battle",
            Self::BattleSwitch => "battle-switch",
            Self::Shop => "shop",
            Self::Pc => "pc",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this scene takes a palette at all.
    ///
    /// False for [`SceneId::Title`], where `docs/design/macros.md` section 2 says the readout's
    /// boot variant applies instead: that is the one scene in which the fly's raw buttons still
    /// reach the cartridge in palette mode, which is what lets Start fire through the intro.
    pub const fn playable(self) -> bool {
        !matches!(self, Self::Title)
    }
}

/// One bound slot, for the stage's palette strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotBinding {
    /// 0..[`SLOTS`], in the readout's D-pad-then-A/B order.
    pub slot: u8,
    /// The macro's name, at most fourteen characters.
    pub name: &'static str,
    /// Two or three words of what it does in this scene ("nearest door").
    pub gloss: &'static str,
    /// The macro type's own decoder channel and rate role, `macro_<type>`
    /// (`docs/design/macros.md` sections 11 and 12).
    ///
    /// What makes this binding a *button on the pad* rather than a meaning laid over one of the
    /// eight real buttons: the sim loop hands the decoder the channels of the bound slots and the
    /// decoder picks among exactly those.
    pub channel: &'static str,
    /// The channel's short tag for the screen, `MB·GO`, `MB·ATK` and so on.
    pub tag: &'static str,
}

/// What one frame's observation says: the scene, and what each channel means in it.
///
/// Unbound slots are simply absent from `bindings`, which is what the feed contract asks for
/// ("unbound slots omitted") and what the stage draws dim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub scene: SceneId,
    pub bindings: Vec<SlotBinding>,
}

/// How a macro ended: `docs/design/macros.md` section 5's `outcome` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Blocked,
    Timeout,
    Refused,
}

impl Outcome {
    /// The lower-case word the feed event label and `game.macroOutcome` use.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Timeout => "timeout",
            Self::Refused => "refused",
        }
    }
}

/// What [`MacroPalette::start`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// The macro owns the buttons now; this is its name.
    Running(&'static str),
    /// Nothing was pressed. `name` is `None` when the slot was unbound in this scene, which is
    /// "no action" rather than a failure and is not worth an event; `Some` when a bound macro
    /// refused, which is an outcome the feed reports.
    Refused { name: Option<&'static str>, reason: &'static str },
}

/// A macro palette over one game, as the sim loop sees it.
///
/// The frame order is the loop's (`docs/design/flysim.md` section 4): [`MacroPalette::start`] and
/// [`MacroPalette::step`] are called before the emulator frame, where the raw button mask would be
/// applied, and [`MacroPalette::observe`] after it, which is where
/// `docs/design/macros.md` section 2 puts scene detection ("sampled once per game frame after the
/// frame"). Every method reads WRAM through `memory` and none of them writes anything.
/// `ledger` is the adapter's exploration ledger, which is what makes a way out's "nearest
/// *unvisited* exit" answerable; it is passed per call rather than held, because the adapter owns
/// it and it changes under the palette as the fly explores.
pub trait MacroPalette: Send {
    /// The brain clock, handed over before the calls that read it.
    ///
    /// The one thing a palette needs that is neither WRAM nor the adapter's ledger: section 12's
    /// blocked-target ledger excludes a target for ten *brain* minutes, and brain minutes are the
    /// loop's clock rather than anything the cartridge knows. Defaulted to a no-op because a
    /// palette with no clock of its own is a complete implementation, and passed rather than
    /// sampled here so that every question one frame asks is asked at one instant.
    fn clock(&mut self, ms: f64) {
        let _ = ms;
    }

    /// Detect the scene and bind its slots. Called once per frame, after the frame.
    fn observe(&mut self, memory: &mut dyn MemoryReader, ledger: &dyn RunLedger) -> Observed;

    /// Begin the macro in `slot` of the palette the last [`MacroPalette::observe`] bound, or
    /// refuse and press nothing.
    fn start(&mut self, slot: u8, memory: &mut dyn MemoryReader, ledger: &dyn RunLedger)
    -> Started;

    /// The mask to hold this frame, or `None` when the running macro has just finished.
    fn step(&mut self, memory: &mut dyn MemoryReader, ledger: &dyn RunLedger) -> Option<u8>;

    /// The name of the macro that owns the buttons, if any.
    fn running(&self) -> Option<&'static str>;

    /// How the last macro ended, taken: a second call returns `None` until another one ends.
    fn take_finished(&mut self) -> Option<(&'static str, Outcome)>;

    /// Give up on whatever is running, because the sim loop is rolling the game back. The
    /// abandoned macro is reported by the next [`MacroPalette::take_finished`].
    fn cancel(&mut self);

    /// Whether the last [`MacroPalette::observe`] saw the fly **nearer its objective than it has
    /// been** since that objective was set, measured in map hops.
    ///
    /// The ratchet's stall window is reset by exploration -- one new tile
    /// (`docs/design/ladder.md`, the 2026-09-17 progress rule) -- and a fly crossing a town it
    /// has already covered to reach the rung's own door earns no new ground while it does it.
    /// That is the rung-10 stall of 2026-09-22 in one line: two "Stuck" rollbacks inside half an
    /// hour, both of them on a fly that was walking, both of them landing it back where it had
    /// started. Getting nearer the objective than this run has ever been is the other thing that
    /// is plainly progress, and it is a *level* rather than a counter so nothing is checkpointed
    /// and nothing can drift: it is true on the frame the distance falls and false after.
    ///
    /// Read by the sim loop and by nothing else. No macro is ranked by it, no button is bound on
    /// it and it presses nothing (`docs/design/macros.md` section 12): it is the loop's own
    /// answer to "is this run getting somewhere".
    ///
    /// The default is `false`, which is a palette with no objective to be nearer to.
    fn nearer_the_objective(&self) -> bool {
        false
    }
}

/// Every macro channel a game's palette can ever bind, in the contract's own order, or empty for
/// a game with no palette.
///
/// The decoder's `macros` exclusive group is built from this (`docs/design/macros.md` section 11):
/// every type has a channel for the whole run, whether or not the scene on screen binds it, so a
/// channel means the same action from one scene to the next and learning can attach to it. Which
/// of them may win a given decision is the scene's business and travels per decision, as the
/// bound set.
pub fn macro_channels(game: &str) -> Vec<&'static str> {
    match game {
        "pokemon-red" => crate::pokemon_red::macros::palette::MacroKind::BY_CHANNEL
            .iter()
            .map(|kind| kind.channel())
            .collect(),
        _ => Vec::new(),
    }
}

/// The macro palette for a game id, or `None` for a game that has none.
///
/// Only Pokémon Red has one: `docs/design/macros.md` is written against the pinned pokered
/// commit's WRAM, and the platformer has no palette at all, so palette mode over the platformer
/// is a configuration the sim loop refuses rather than a palette that guesses. `seed` is carried
/// for a macro with a random component; nothing in the palette has one since `GO FRONTIER`
/// replaced `WANDER` (section 9), and it stays so a seeded run is reproducible if one ever does.
pub fn palette_for(game: &str, seed: u32, mode: PaletteMode) -> Option<Box<dyn MacroPalette>> {
    match game {
        "pokemon-red" => Some(Box::new(
            crate::pokemon_red::macros::driver::PokemonPalette::with_mode(seed, mode),
        )),
        _ => None,
    }
}

/// Which of the two non-raw modes a palette is dealt for (`docs/design/macros.md` sections 3
/// and 9).
///
/// The same six slots either way, and the same executor behind them. What differs is what a slot
/// *is*: in [`PaletteMode::Palette`] it is a channel of the readout with a fixed meaning in this
/// scene, and in [`PaletteMode::Plan`] it is a rank in the scene's plan, ordered by the policy,
/// with the readout choosing when and how far down the order to go rather than which entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PaletteMode {
    /// Slot = channel: UP is slot 0, DOWN 1, LEFT 2, RIGHT 3, A 4, B 5.
    #[default]
    Palette,
    /// Slot = rank in the scene's plan, best first.
    Plan,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_macro_type_has_a_channel_and_a_tag_and_the_channel_is_its_name() {
        use crate::pokemon_red::macros::palette::MacroKind;

        // One list, three uses (`MacroKind::BY_CHANNEL`): the order the populations were cut in,
        // the decoder's channel order and the screen's cell order. A type missing from it would
        // be a type with no button.
        assert_eq!(MacroKind::BY_CHANNEL.len(), MacroKind::ALL.len());
        for kind in MacroKind::ALL {
            assert!(MacroKind::BY_CHANNEL.contains(&kind), "{:?}", kind.name());
        }

        // The channel is the rate role is the name: `macro_` plus the macro's name lowercased
        // with spaces as underscores. `tools/build_flywire.py` writes the roles by that rule and
        // the stage finds a bound macro's rate by it, so all three ends are pinned here.
        let mut channels: Vec<&str> = Vec::new();
        let mut tags: Vec<&str> = Vec::new();
        for kind in MacroKind::BY_CHANNEL {
            let expected = format!("macro_{}", kind.name().to_lowercase().replace(' ', "_"));
            assert_eq!(kind.channel(), expected, "{}", kind.name());
            assert!(kind.channel_tag().starts_with("MB·"), "{}", kind.channel_tag());
            assert!(
                kind.channel_tag().chars().count() <= 8,
                "{} is too wide for the glyph column",
                kind.channel_tag()
            );
            channels.push(kind.channel());
            tags.push(kind.channel_tag());
        }
        let unique_channels: std::collections::BTreeSet<&&str> = channels.iter().collect();
        assert_eq!(unique_channels.len(), channels.len(), "two types share a channel");
        let unique_tags: std::collections::BTreeSet<&&str> = tags.iter().collect();
        assert_eq!(unique_tags.len(), tags.len(), "two types share a tag");

        // The group the decoder is built from is exactly those channels, in that order, and no
        // other game has one.
        assert_eq!(macro_channels("pokemon-red"), channels);
        assert!(macro_channels("platformer").is_empty());
    }

    /// The tags the page draws are the tags this crate publishes.
    ///
    /// `packages/feed/src/types.ts` carries the same twenty-two rows because a consumer has to be
    /// able to order the cells by type without asking the producer; this reads that table and
    /// compares it, so a rename on either side fails a test rather than showing a blank glyph on
    /// air. The same trick `chat-cases.json` plays for the sanitizer, minus the fixture.
    #[test]
    fn the_feed_packages_macro_table_is_this_crates_channel_order() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../packages/feed/src/types.ts");
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("skipped: {path} is absent in this worktree");
            return;
        };
        let table = text
            .split_once("const MACRO_TABLE")
            .and_then(|(_, rest)| rest.split_once("];"))
            .map(|(table, _)| table)
            .expect("types.ts must carry MACRO_TABLE");
        let rows: Vec<(String, String)> = table
            .lines()
            .filter_map(|line| line.trim().strip_prefix('['))
            .filter_map(|line| line.split_once(','))
            .map(|(name, tag)| {
                let clean = |value: &str| value.trim().trim_matches([',', ']', '\'']).to_string();
                (clean(name), clean(tag))
            })
            .collect();
        let expected: Vec<(String, String)> = crate::pokemon_red::macros::palette::MacroKind::BY_CHANNEL
            .iter()
            .map(|kind| (kind.name().to_string(), kind.channel_tag().to_string()))
            .collect();
        assert_eq!(rows, expected);
    }

    #[test]
    fn the_feed_scene_names_are_the_closed_set_the_protocol_publishes() {
        let names: Vec<&str> = SceneId::ALL.iter().map(|scene| scene.feed_name()).collect();
        assert_eq!(
            names,
            [
                "title",
                "overworld",
                "dialog",
                "menu",
                "battle",
                "battle-switch",
                "shop",
                "pc",
                "unknown"
            ]
        );
        // The one scene with no palette, where the raw boot readout still applies.
        assert!(!SceneId::Title.playable());
        assert!(SceneId::ALL.iter().skip(1).all(|scene| scene.playable()));
    }

    #[test]
    fn only_pokemon_has_a_palette() {
        for mode in [PaletteMode::Palette, PaletteMode::Plan] {
            assert!(palette_for("pokemon-red", 1, mode).is_some());
            assert!(palette_for("platformer", 1, mode).is_none());
            assert!(palette_for("sonic", 1, mode).is_none());
        }
    }

    #[test]
    fn the_outcome_words_are_the_ones_the_contract_spells() {
        assert_eq!(Outcome::Done.label(), "done");
        assert_eq!(Outcome::Blocked.label(), "blocked");
        assert_eq!(Outcome::Timeout.label(), "timeout");
        assert_eq!(Outcome::Refused.label(), "refused");
    }
}

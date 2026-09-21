# Milestone ladder v2 for Pokémon Red

Planned 2026-09-15 (Fable). Replaces the 16-rung ladder (boot..Pokédex, then one rung per badge)
with a 38-rung ladder that follows the game's real progression, so the ratchet archives every
hour or two of progress instead of every badge, and the stream's rung panel moves visibly.

**Built 2026-09-15.** Three things came back different from the plan; the table below is corrected
in place and the corrections are recorded under "Verified against the decomp".

Sources: pret/pokered at the pinned commit 0cd19d3b877b7dc66d12c7050bed9a7f38154d4b
(`constants/event_constants.asm`, `constants/map_constants.asm`, `ram/wram.asm`), the
generated symbol table in `services/flysim/crates/flybrain-gb/src/pokemon_red/symbols.rs`, and the
existing rank logic in `pokemon_red/mod.rs`. Every flag name below has been verified against the
decomp at that commit.

## Rules

- Rank is the maximum over all satisfied rung conditions, evaluated on every playable sample.
  Conditions are sticky by construction: badge bits only grow, and "first visit to map X" is read
  from the adapter's lifetime map ledger, not the current map. So rank never regresses, and rungs
  may be earned out of order (a fly that stumbles into Viridian Forest before Viridian City still
  gets the higher rung).
  - Event flags, however, are **not** all sticky — the plan was wrong about this, see below — so
    every flag rung is answered from the adapter's lifetime `seen` ledger rather than from the live
    bit. The ledger already records each flag under its pokered name the first sample it is seen
    set, survives a rollback and round-trips through the checkpoint, so this needed no new state.
- Rungs based on a map use FIRST VISIT (the map id enters the lifetime ledger), never current
  position, and the safe-snapshot rules stay exactly as today (overworld, unscripted, three stable
  samples, no menu/battle/warp). The archive for a map rung is therefore taken a few seconds after
  arrival, standing still.
- Badges come from `wObtainedBadges` bit count as today. Elite Four rungs use the
  `EVENT_BEAT_LORELEIS_ROOM_TRAINER_0`-style flags, whose names were confirmed, and Champion is
  `EVENT_BEAT_CHAMPION_RIVAL` seen once **or** `wNumHoFTeams > 0`: neither alone is enough, see
  below.
- Adapter version bumps to `pokered-unique8-v4`; the ratchet's rank bound becomes the ladder length;
  old checkpoints are rejected (pre-launch, acceptable). The feed header gains
  `milestone.total` (optional, non-breaking) so the page draws the right number of rungs.

## The ladder

| rank | label | condition |
|---:|---|---|
| 0 | Boot screen | not yet playable |
| 1 | Bedroom | first playable sample (map REDS_HOUSE_2F) |
| 2 | Downstairs | first visit REDS_HOUSE_1F |
| 3 | Pallet Town | first visit PALLET_TOWN |
| 4 | Oak's lab | first visit OAKS_LAB (or EVENT_FOLLOWED_OAK_INTO_LAB) |
| 5 | Got a starter | EVENT_GOT_STARTER |
| 6 | Oak's parcel | EVENT_OAK_GOT_PARCEL (the parcel *delivered*, not collected) |
| 7 | Pokédex | EVENT_GOT_POKEDEX |
| 8 | Viridian City | first visit VIRIDIAN_CITY |
| 9 | Viridian Forest | first visit VIRIDIAN_FOREST |
| 10 | Pewter City | first visit PEWTER_CITY |
| 11 | Boulder Badge | badges >= 1 |
| 12 | Mt. Moon | first visit MT_MOON_1F |
| 13 | Cerulean City | first visit CERULEAN_CITY |
| 14 | Cascade Badge | badges >= 2 |
| 15 | Nugget Bridge | EVENT_BEAT_CERULEAN_RIVAL (rival north of Cerulean) |
| 16 | Met Bill | EVENT_GOT_SS_TICKET (added to the generated symbols) |
| 17 | Vermilion City | first visit VERMILION_CITY |
| 18 | HM Cut | EVENT_GOT_HM01 |
| 19 | Thunder Badge | badges >= 3 |
| 20 | Rock Tunnel | first visit ROCK_TUNNEL_1F |
| 21 | Lavender Town | first visit LAVENDER_TOWN |
| 22 | Celadon City | first visit CELADON_CITY |
| 23 | Silph Scope | EVENT_BEAT_ROCKET_HIDEOUT_GIOVANNI — there is no EVENT_GOT_SILPH_SCOPE |
| 24 | Rainbow Badge | badges >= 4 |
| 25 | Poké Flute | EVENT_GOT_POKE_FLUTE (Mr. Fuji rescued) |
| 26 | Fuchsia City | first visit FUCHSIA_CITY |
| 27 | Soul Badge | badges >= 5 |
| 28 | Silph Co. freed | EVENT_BEAT_SILPH_CO_GIOVANNI |
| 29 | Marsh Badge | badges >= 6 |
| 30 | Cinnabar Island | first visit CINNABAR_ISLAND |
| 31 | Volcano Badge | badges >= 7 |
| 32 | Earth Badge | badges >= 8 |
| 33 | Indigo Plateau | first visit INDIGO_PLATEAU_LOBBY |
| 34 | Beat Lorelei | EVENT_BEAT_LORELEIS_ROOM_TRAINER_0, latched (the flag is reset) |
| 35 | Beat Bruno | EVENT_BEAT_BRUNOS_ROOM_TRAINER_0, latched |
| 36 | Beat Agatha | EVENT_BEAT_AGATHAS_ROOM_TRAINER_0, latched |
| 37 | Champion | EVENT_BEAT_CHAMPION_RIVAL latched, or `wNumHoFTeams > 0` |

38 rungs, 0..37. Labels are what the rung panel shows; keep them short (they render in Press Start 2P
at 33 px in a 1012 px rail).

## Verified against the decomp

All 16 map constants and all but one flag name were confirmed unchanged at 0cd19d3. The
exceptions, and one wrong assumption:

- **`EVENT_GOT_SILPH_SCOPE` does not exist** (rung 23). The Silph Scope is a toggleable pick-up
  that `RocketHideoutB4FBeatGiovanniScript` reveals — `data/maps/toggleable_objects.asm` has
  `ROCKETHIDEOUTB4F_SILPH_SCOPE` starting `OFF`. So the rung is
  `EVENT_BEAT_ROCKET_HIDEOUT_GIOVANNI`, which is the plan's own parenthetical.
- **`EVENT_GOT_SS_TICKET` was missing from the generated symbols** (rung 16), and from the
  prototype's `symbols.ts` too, so `gen_symbols.py` gained an explicit allowlist resolved from
  `event_constants.asm`. Bit 1372. `EVENT_MET_BILL` (bit 1368) also exists; the ticket is the rung
  because it is the S.S. Anne gate, i.e. the progression.
- **Rung 6 is the delivery**, `EVENT_OAK_GOT_PARCEL`, not `EVENT_GOT_OAKS_PARCEL`. It is what
  unlocks the Pokédex, so it sits directly below rung 7. This is a change from v3.

### The Elite Four and the Champion are not sticky

The plan's "event flags never clear in normal play" is false for exactly the top four rungs, and
these are the only two `ResetEventRange` calls in the game:

- `scripts/IndigoPlateauLobby.asm:14` runs `ResetEventRange INDIGO_PLATEAU_EVENTS_START,
  EVENT_LANCES_ROOM_LOCK_DOOR` on **every** entry to the lobby once `BIT_STARTED_ELITE_4` is set in
  `wElite4Flags`. That clears Lorelei's, Bruno's, Agatha's and Lance's beat flags — which is what
  happens after a blackout, the single most likely thing to happen to a fly in the Elite Four.
- `scripts/HallOfFame.asm:45` runs `ResetEventRange INDIGO_PLATEAU_EVENTS_START,
  INDIGO_PLATEAU_EVENTS_END, 1`, which covers `EVENT_BEAT_CHAMPION_RIVAL` as well: the flag for the
  ladder's last rung is destroyed by the event that earns it.

So `wNumHoFTeams` (0xd5a2, incremented by `HallOfFamePC` and saved) is the only durable
"has been Champion" signal on the cartridge — but it is set slightly *after* the rival is beaten,
and only the flag marks that moment. Hence both halves.

Rather than special-case four rungs, **every** flag rung reads the adapter's lifetime `seen` ledger.
The adapter already walks `symbols::EVENTS` on every playable sample, battles included, and records
each set flag under its pokered name whether or not it pays; that record survives
`clear_transient` and round-trips through the checkpoint. The flags are latched during the
thousands of frames between the battle ending and the reset firing, so the ledger is the sticky
condition the plan wanted, at the cost of no new state.

## Recovery budgets

Attempts per rung stay 3; lifetime budget scales with the ladder (36 instead of 12). Stall window
unchanged (120 s without new exploration, 180 s since last recovery). The ratchet's rank bound is
now the running adapter's ladder length, passed in rather than a constant, because the bound belongs
to the adapter: Pokémon's ladder is 38 rungs and the platformer's is 16
(`docs/design/platformer.md` §3). Only the *budgets* are per-game, from the adapter's
`RecoveryPolicy`: the platformer asks for 48 lifetime recoveries, a 300 s stall window and a
game-over trigger with its own 60 s cooldown, and Pokémon takes the defaults above.

## Verification

Unit tests with synthetic WRAM traces for: each map rung on first visit only; out-of-order rungs
take the max; badge rungs; flag rungs; rank never decreases across a rollback; ladder length equals
labels length equals 38; old adapter version rejected on import. The ROM-gated boot test asserts
rank 1 in the bedroom and rank 3 on reaching Pallet Town as before.

All present, in `pokemon_red/tests.rs`, plus two the corrections above made necessary: the Elite
Four rungs surviving a simulated lobby reset, and both halves of the Champion rung. Also
`ratchet.rs` for the ladder-derived rank bound, `simloop.rs` for the `next` label at the top of the
ladder, `packages/feed/tests/schema.test.ts` for `milestone.total`, and two stage suites —
`tests/unit/ladder.test.ts` for the rung count and `tests/e2e/ladder.spec.ts`, which drives the real
build's spine to 38 rungs at 1080 and measures it (gap 1.25 px, segments 14.2 px, the current-rung
marker 20 px, which is 4.1 px at the phone downscale).

The ROM-gated boot test now walks out of the house on the real cartridge as well, and asserts the
climb is exactly (2, DOWNSTAIRS) then (3, PALLET TOWN). It does so with a fixed-seed random walk:
steering needs a collision map to be any good, and the player spawns at (3, 6) in the bedroom with
(3, 5) blocked, so aimed steering stalls on the furniture.

No screenshot baseline changed. The committed `.flyfeed` fixtures predate `milestone.total`, so the
page's spine is still 16 rungs there, and the gap formula (`48 / rungs`, exactly 3 px at 16) and the
marker floor (never binding at 16) were both chosen to leave that rendering identical. Re-recording
the fixtures against the now-38-rung fake flysim would move every baseline and is a separate change.

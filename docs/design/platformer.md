> Design document produced 2026-09-15 by a planning agent. Post-MVP; the Rust GameAdapter trait and the stage per-game config are shaped so this drops in.

# Design: the platformer adapter (demo 2)

Status: design, 2026-09-15. Read-only research pass. Every RAM address below carries a source.
Anything I could not verify from source is marked **UNVERIFIED**.

Companion docs: `docs/stream-mvp-plan.md` (decisions), `docs/streaming-plan.md` §6 (superseded in part
by §1 below), `docs/readout.md`, `docs/integration.md`, and the Pokemon adapter at
`~/fly-plays-pokemon/src/reward/{catalog.ts,pokemon-red.ts}` and `src/runtime/ratchet.ts`.

## 1. Game choice: Super Mario Land, primary. Kirby's Dream Land, fallback.

`streaming-plan.md` §6 recommended SML "if the world/stage and scroll search stalls, fall back to
Kirby", because Data Crystal only lists world/stage as VRAM HUD tile indices and lists no global
scroll ([SML RAM map](https://datacrystal.tcrf.net/wiki/Super_Mario_Land/RAM_map)). **That search is
already done.** Kasper Meerts' disassembly names all of it in HRAM:

| Symbol | Address | Source |
|---|---|---|
| `hGameState` | `0xFFB3` | [hram.asm](https://github.com/kaspermeerts/supermarioland/blob/master/hram.asm) |
| `hWorldAndLevel` (BCD nibbles, 1-1 = `0x11`) | `0xFFB4` | hram.asm; encoding at [bank0.asm:711, 746](https://github.com/kaspermeerts/supermarioland/blob/master/bank0.asm) |
| `hLevelIndex` (0..11) | `0xFFE4` | hram.asm; `cp a, $0C ; 12 levels in total` bank0.asm:700-702 |
| `hScreenIndex` | `0xFFE5` | hram.asm; incremented every 20 columns, bank0.asm:5215-5223 |
| `hColumnIndex` (0..19) | `0xFFE6` | hram.asm; `cp a, $14 ; 20 columns per screen?` bank0.asm:5217 |
| `hScrollX` (written straight to `rSCX`) | `0xFFA4` | hram.asm; bank0.asm:2775-2776 |
| `hCoins` (BCD, 0..99) | `0xFFFA` | hram.asm |
| `hSuperStatus` | `0xFF99` | hram.asm; 0 small, 1 growing, 2 super, 3+ i-frames (bank0.asm:1116, 1380-1385, 1408 `InjureMario`) |
| `hSuperballMario` | `0xFFB5` | hram.asm |
| `hGamePaused` | `0xFFB2` | hram.asm; bank0.asm:1083-1090 |
| `hWinCount` (game clears) | `0xFF9A`, mirror `wWinCount 0xC0E1` | hram.asm, wram.asm; incremented at bank0.asm:3141-3144 after "THE END" |
| `wScore` (3 bytes BCD) | `0xC0A0` | [wram.asm](https://github.com/kaspermeerts/supermarioland/blob/master/wram.asm) |
| `wLives` | `0xDA15` | wram.asm |
| `wLivesEarnedLost` | `0xC0A3` | wram.asm; `+1` on 1UP (bank0.asm:1399), `0xFF` on death (bank0.asm:925) |
| `wGameTimer` (3 bytes) / `wGameTimerExpiringFlag` | `0xDA00` / `0xDA1D` | wram.asm |
| `wGameOverWindowEnabled` / `wGameOverTimerExpired` | `0xC0A5` / `0xC0AD` | wram.asm |
| `wInvincibilityTimer` | `0xC0D3` | wram.asm |

`hGameState` is a full jump table, listed verbatim at bank0.asm:2A6: `00` normal gameplay, `01` dead,
`02` reset to checkpoint, `03` pre-dying, `04` dying animation, `05` explosion / score countdown,
`06` end of level, `07` end-of-level gate, `08` increment level and load tiles, `09`-`0C` pipe
transitions, `0D` autoscrolling level, `0E` init menu, `0F` start menu, `11` level start, `12` bonus
game. Beyond the printed table there are labelled handlers up to `GameState_3C`; `0x39` prepares the
"game over" text and `0x3A` is the game-over wait (bank0.asm:4291-4348, and `cp a, $3A / jr nz, .out
; game not over` at bank0.asm:2784-2786). Data Crystal's "`0x39` = Game Over" is therefore roughly
right and the disassembly explains why.

Level length is also known statically. `levels/levels.asm` lists one screen pointer per screen,
terminated by `db $ff`: 1-1 = 18, 1-2 = 17, 1-3 = 18, 2-1 = 19, 2-2 = 17, 2-3 = 21, 3-1 = 26,
3-2 = 19, 3-3 = 18, 4-1 = 26, 4-2 = 23, 4-3 = 27
([levels.asm](https://github.com/kaspermeerts/supermarioland/blob/master/levels/levels.asm)). Levels
start at `hScreenIndex = 3` (`ld a, $03 / ldh [hScreenIndex], a ; do all levels start on screen 3`,
bank0.asm:2071). Death restores `hScreenIndex` to one of `03, 07, 0B, 0F, 13, 17` with matching
`0xC0AB` values `0C, 34, 5C, 84, AC, D4` (`GameState_02`, bank0.asm:952-978), so the checkpoint grid
is every four screens and is exactly known.

So **a monotone in-level progress signal exists with no VRAM read**:
`column = hScreenIndex * 20 + hColumnIndex`, `percent = (column - 60) / ((screens[level] - 3) * 20)`.
`0xC0AB` is a single-byte alternative, commented "sort of progress in the level in columns / 2"
(bank0.asm:3361), but it is unnamed; prefer the two named HRAM bytes.

### Is reading VRAM acceptable? No, and we do not need to.

Now verified, not unverified. binjgb's `emulator_read_mem` calls `read_u8_raw` -> `read_u8_pair(...,
raw=TRUE)`, and the `MEMORY_MAP_VRAM` case still goes through `read_vram`, which returns
`INVALID_READ_BYTE` when `is_using_vram(e, FALSE)` (PPU mode 3, plus the tick before it via
`is_almost_mode3`). The `raw` flag only suppresses ROM hooks
([src/emulator.c](https://github.com/binji/binjgb/blob/main/src/emulator.c), `read_vram` ~L1674,
`read_u8_pair` ~L1957, `emulator_read_mem` ~L5194). By contrast `emulator_get_wram_ptr` and
`emulator_get_hram_ptr` hand back raw pointers with no PPU gating (~L5186-5192). Decision: read WRAM
and HRAM only, ideally through the bulk pointers; treat any VRAM read as a bug.

### The rejected candidates

- **Wario Land: Super Mario Land 3.** The [Kak2X/wl](https://github.com/Kak2X/wl) disassembly is the
  best-labelled of the five: `sLevelId 0xA804`, `sTotalCoins 0xA805-0xA807`, `sHearts 0xA808`,
  `sLives 0xA809`, `sLevelsCleared 0xA80D`, `sGameMode 0xA8C3`, a true 16-bit global
  `sLvlScrollX 0xA902/0xA903`, `sLevelCoins 0xA97A/0xA97B`, `sPaused 0xA908`, `sBossRoom 0xA995`
  ([src/memory.asm](https://github.com/Kak2X/wl/blob/master/src/memory.asm)). Rejected anyway: that
  state lives in cartridge SRAM, and binjgb's `gb_read_ext_ram` returns `INVALID_READ_BYTE` whenever
  `ext_ram_enabled` is clear (emulator.c ~L1243), so every sample would need the MBC1 RAM gate open;
  and Wario Land gates level entry behind a walkable world map, which a mostly-random walker will not
  navigate. Keep it as the *third* option if SRAM turns out to be readable every frame.
- **Donkey Kong '94.** No Data Crystal RAM map (only Donkey Kong Land / Land III exist) and no
  disassembly with named RAM found. It is also a 101-stage puzzle-platformer where progress needs a
  key carried to a door; a random walker earns nothing. Deprioritised, agreeing with §6.
- **Mega Man: Dr. Wily's Revenge / Castlevania: The Adventure (GB).** No complete named-RAM
  disassembly found; both are among the hardest GB games, and Castlevania's whip plus its slow
  movement means a random walker dies in the first screen. Reject.
- **Kirby's Dream Land: the fallback.** [huderlem/kirbydreamland](https://github.com/huderlem/kirbydreamland)
  names, in WRAM: `wCurStage 0xD03B`, `wCurStageScreen 0xD03E`, `wStageLengthInMetatiles 0xD042`
  ("prevents scrolling too far to the right"), `wStageScrollTileX 0xD051`, `wStageScrollTileY
  0xD052`, `wPlayerScreenXCoord 0xD05C`, `wPlayerScreenYCoord 0xD05D`, `wRemainingLives 0xD089`,
  `wMaximumLives 0xD08A`, `wScore 0xD08B` (3 bytes, little-endian)
  ([wram.asm](https://github.com/huderlem/kirbydreamland/blob/master/wram.asm)). Health at `0xD086`
  is used as a bare literal (`bank_000.asm:173`) and is named only by
  [Data Crystal](https://datacrystal.tcrf.net/wiki/Kirby's_Dream_Land:RAM_map): **UNVERIFIED**. Kirby
  also has no game-mode symbol in that disassembly, so the playable gate would need its own RAM
  search. Its real advantage is forgiveness: Kirby floats and has a health bar, so a random walker
  survives far longer.

### Verdict

Super Mario Land wins on RAM-map completeness (now equal to Kirby, better on game state),
recognisability (a dedicated Twitch category, the sibling-of-Pokemon framing lands instantly), and
the fact that its game timer kills a standing-still fly automatically, which keeps the stream moving
without any stall penalty. Kirby wins only on forgiveness. IP risk is identical: SML is Nintendo
R&D1 / Nintendo, Kirby's Dream Land is HAL / Nintendo, Wario Land is Nintendo
([Super Mario Land, Mario Wiki](https://www.mariowiki.com/Super_Mario_Land)). Same posture as
Pokemon Red: local ROM only, gitignored, SHA-256 pinned, never shown, never linked.

Adapter identity: `sml-progress-v1`. ROM pin: SHA-256 of the local copy of Super Mario Land (World)
(Rev A); the disassembly builds SHA-1 `418203621b887caa090215d97e3f509b79affd3e`
([rom.sha1](https://github.com/kaspermeerts/supermarioland/blob/master/rom.sha1)). Other revisions
are **UNVERIFIED** for layout; anything else shows `SEMANTIC REWARDS OFF`, exactly as
`pokemon-red.ts:43` does.

## 2. Reward catalog

Same shape as `src/reward/catalog.ts`. All values positive. No penalties.

| Kind | Label | Trigger | Value | Budget / cap | Stim ms |
|---|---|---|---|---:|---:|
| `started` | Start | First boot -> playable transition observed | 1 | once per lifetime | 250 |
| `band` | Ground | Each new 10-column band of the current level reached (`column = hScreenIndex*20 + hColumnIndex`) | 0.05 | keyed `band:<level>:<n>` in the lifetime ledger; at most `2*(screens-3)` per level (30 for 1-1, 48 for 4-3) | 80 |
| `coin` | Coin | `hCoins 0xFFFA` BCD increases (mod-100 wrap counts as +1) | 0.02 | 40 payouts per level, lifetime | 60 |
| `score` | Points | `wScore 0xC0A0` 3-byte BCD increases | `0.05 * min(1, delta/400) / (1 + floor(n/4))` where `n` = prior payouts in this level | 20 payouts per level, lifetime | 80 |
| `powerup` | Power-up | `hSuperStatus 0xFF99` reaches 2, or `hSuperballMario 0xFFB5` becomes nonzero | 0.5 | first of each per level, lifetime | 200 |
| `life` | 1UP | `wLives 0xDA15` increases | 1 | uncapped (rare: 100 coins or a 1UP mushroom) | 250 |
| `level` | Level | `hLevelIndex 0xFFE4` rises past its lifetime max (cross-checked against `hWorldAndLevel`) | 3 | once per level, lifetime | 400 |
| `world` | World | High nibble of `hWorldAndLevel 0xFFB4` rises past its lifetime max | 5 | once per world, lifetime | 500 |
| `clear` | Game clear | `hWinCount 0xFF9A` rises | 10 | once per lifetime | 800 |

`score` deliberately stands in for "enemy defeated". Stomps pay through score floaties
(`hStompChain 0xFF9D`, chain capped at 3, `hStompChainTimer 0xFF9C` = 50 frames, bank0.asm:1285-1301),
and breakable blocks call `AddScore` with `de = 0x0050` (bank0.asm:3917). Decoding the 160-byte enemy
object table at `0xD100` to prove a kill is strictly harder and buys nothing the score delta does not
already give.

### Playable gate

All of these, every sample, before any payout:

1. `hGameState 0xFFB3` in `{0x00, 0x0D}` (normal gameplay, autoscroll). Excludes dying (`03`/`04`),
   score countdown (`05`), end of level (`06`/`07`), level load (`08`), pipes (`09`-`0C`), menus
   (`0E`/`0F`), level start (`11`), bonus game (`12`) and game over (`39`/`3A`).
2. `hGamePaused 0xFFB2 == 0`.
3. `0xFF9F == 0`. bank0.asm:341 comments it "Equal to 28 in menu and during demo", and `Call_2113`
   (bank0.asm:5060-5066) overwrites `hJoyHeld` from `0xC0DB` only when `0xFF9F` is nonzero, so this
   is the attract-demo gate. **Name UNVERIFIED** (unnamed in `hram.asm`); confirm with a watchpoint
   before shipping. Without it the attract demo would farm rewards.
4. `wGameOverWindowEnabled 0xC0A5 == 0`.
5. Consistency: high nibble of `hWorldAndLevel` in 1..4, low nibble in 1..3, `hLevelIndex < 12`, and
   `hLevelIndex == (world-1)*3 + (level-1)`. A mismatch means a mid-transition or wrong-revision
   read; report `TRANSITION` and pay nothing. Cheap, and it catches a wrong ROM immediately.
6. `hScreenIndex` in `[3, screens[level]]`, `hColumnIndex < 20`, `wLives >= 1`.
7. Three consecutive stable samples in the same level (mirrors `pokemon-red.ts:122-125`'s `stable >= 3`).

### Baselining

On the first valid sample (`initialized === false`), record without paying: the current level, every
band key up to the current column, `hCoins`, `wScore`, `wLives`, `hSuperStatus`, `hSuperballMario`,
`hLevelIndex`, world, `hWinCount`. A restored checkpoint mid-level therefore replays nothing, exactly
as `pokemon-red.ts:85-92` does. `started` pays only if a boot state was observed first
(`sawBoot`).

### Novelty caps so oscillation cannot farm

Bands are keyed positions in a lifetime `Set`, not "beat the previous best by N pixels". Walking left
and right across a band boundary pays once, ever. Dying and re-running the same ground pays nothing:
progress inside the level resets, the ledger does not. Coin and score payouts are per-level lifetime
counters, so a coin that respawns after a death can be re-collected but can only be paid 40 times in
that level for the whole run. `score` additionally decays as `1/(1 + floor(n/4))`, mirroring the
wild-KO decay at `pokemon-red.ts:116`.

Death pays nothing and costs nothing. This is doctrine, and it is also the anti-farm mechanism: the
only repeatable income is capped, and the only large income is ground never reached before.

### Safe snapshot (for the ratchet)

Everything in the playable gate, plus: `hGameState == 0x00` exactly (not `0x0D`; restoring an
autoscroll vehicle level mid-flight is fragile), `0xC20A == 1` (on the ground; bank0.asm:1258
`ld hl, $C20A ; 1 if on ground`), `0xC207 == 0` (jump status 00 on ground / 01 ascending / 02
descending, bank0.asm:4409), `wInvincibilityTimer 0xC0D3 == 0`, `hSuperStatus` in `{0, 2}` (not 1
growing, not 3+ i-frames), `wGameTimer 0xDA00` BCD >= 100 units, `wGameTimerExpiringFlag 0xDA1D == 0`,
`0xFFF9 == 0` (not underground; comment-only name, **UNVERIFIED**), and the three stable samples.

## 3. Milestone ladder (0..15, 16 ranks)

Keeps the existing `state.best > 15` guard in `ratchet.ts:33` valid unchanged.

| Rank | Label | Condition |
|---:|---|---|
| 0 | BOOTING | no playable sample yet |
| 1 | GAME STARTED | first playable sample |
| 2 | FIRST COIN | any `coin` payout |
| 3 | HALFWAY THROUGH 1-1 | level 0 and `column >= 60 + (18-3)*20/2 = 210` |
| 4 | 1-1 CLEARED | `hLevelIndex` reached 1 |
| 5 | 1-2 CLEARED | reached 2 |
| 6 | WORLD 1 CLEARED (King Totomesu) | reached 3 |
| 7 | 2-1 CLEARED | reached 4 |
| 8 | 2-2 CLEARED | reached 5 |
| 9 | WORLD 2 CLEARED (Dragonzamasu, Marine Pop) | reached 6 |
| 10 | 3-1 CLEARED | reached 7 |
| 11 | 3-2 CLEARED | reached 8 |
| 12 | WORLD 3 CLEARED (Hiyoihoi) | reached 9 |
| 13 | 4-1 CLEARED | reached 10 |
| 14 | 4-2 CLEARED | reached 11 |
| 15 | GAME CLEARED (Tatanga) | `hWinCount` rose |

Formula for ranks 4..15: `4 + highestClearedLevelIndex`. World names and the two vehicle levels are
from [Mario Wiki](https://www.mariowiki.com/Super_Mario_Land) and agree with the disassembly, which
switches to the submarine at `hLevelIndex == 0x05` and the airplane at `0x0B` (bank0.asm:1043-1052).

"First enemy defeated" is deliberately an event-ticker item, not a rank: ranks must be monotone and
must correspond to a state worth archiving, and a stomp is neither.

## 4. Decoder preset `platformer`

New file `packages/brain/src/readout/presets/platformer.ts`, reusing `GAMEBOY_BUTTONS`,
`GAMEBOY_BUTTON_BITS`, `toButtonMask`. Only `DecoderConfig` values change; no decoder code changes.

```
exclusive: {
  channels: { right: 'command_3', left: 'command_2', down: 'command_1', up: 'command_0' },
  decisionMs: 250, holdMs: 250, hysteresis: 1.25, fatigueGain: 0.04, fatigueDecay: 0.85,
},
pulses: [
  { channel: 'a',      role: 'command_4', holdMs: 300, cooldownMs: 420, threshold: 1.10 },
  { channel: 'b',      role: 'command_5', holdMs: 600, cooldownMs: 200, threshold: 1.05 },
  { channel: 'start',  role: 'command_6', holdMs: 55, cooldownMs: 600000, threshold: 2.0,
                       boot: { cooldownMs: 2500, threshold: 1 }, throttleGroup: 'system' },
  { channel: 'select', role: 'command_7', holdMs: 55, cooldownMs: 600000, threshold: 2.0,
                       boot: { cooldownMs: 2500, threshold: 1 }, throttleGroup: 'system' },
],
clearLockoutMs: 300,
```

Rationale, per change against `gameboy.ts`:

- **`holdMs === decisionMs` (250/250, vs 400/400 with an implicit gap).** SML builds momentum in
  `0xC20C` up to 6 and only then sets `0xC20E = 0x02`; with no direction held, momentum decrements
  one per frame and `0xC20E` clears (bank0.asm:4418-4429). Gaps in the right-hold therefore erase
  running speed, and running speed is what clears gaps. 250 ms is about 15 frames, fast enough to
  react to a floor edge, slow enough to be a commitment.
- **Keep all four directions.** `down` enters pipes (`Jmp_1765` tests `hJoyHeld` bit 7,
  bank0.asm:3456-3458) and crouches as Super Mario (bank0.asm:4446-4456). `up` is needed in the two
  vehicle levels. Dropping either would make 2-3 and 4-3 unplayable.
- **`hysteresis` 1.15 -> 1.25.** Reversals in SML trigger a reverse animation and reset momentum
  (bank0.asm:4560-4577). A challenger should need a 25% lead.
- **`fatigueGain` 0.08 -> 0.04, `fatigueDecay` 0.8 -> 0.85.** Habituation exists to break a stuck
  corner in Pokemon. In SML the game timer (`wGameTimer 0xDA00`) breaks a stuck fly for us, so weaker
  habituation is correct: the network should be able to hold right for many seconds.
- **`a` hold 85 -> 300 ms, cooldown 480 -> 420 ms.** Super Mario Land has variable jump height, so a
  85 ms tap is a minimum-height hop and can never clear a two-block gap. The game's own manual is
  the citation ("Press A to jump. To jump higher, press and hold A", quoted on
  [Mario Wiki: Jump](https://www.mariowiki.com/Jump)); the per-frame gravity code lives inside the
  region the disassembly still fills from `baserom.gb` (`GameState_00`, `INCBIN "baserom.gb", $0627,
  $06BC`, bank0.asm:4299-4300), so **the exact hold-to-height curve is UNVERIFIED from source** and
  must be measured. 300 ms is about 18 frames; 420 ms leaves a ~120 ms grounded gap for the landing.
- **`b` as a 600 ms hold with a 200 ms cooldown.** B is run-faster and Superball/vehicle fire
  ([Mario Wiki](https://www.mariowiki.com/Super_Mario_Land)), which needs a *hold*, not a pulse. The
  decoder has no hold primitive outside the exclusive group, but because `cooldownMs < holdMs`, a
  channel whose score stays above threshold refires the instant the hold expires, giving a gapless
  sustained hold that releases within 600 ms of the score falling. Zero decoder changes.
- **`start`/`select` boot-only.** The adapter passes `boot = !playable` (exactly `pokemon-red.ts`'s
  `BOOT` mode), so the permissive 2500 ms / threshold-1 variant applies on the title screen and after
  a game over, letting the fly start a run; during play a 10-minute cooldown at threshold 2.0 makes
  pausing effectively impossible. Pausing is dead air on a 24/7 stream.
- **Keeping both in `throttleGroup: 'system'` is a safety property, not just a throttle.** SML soft
  resets when A, B, Select and Start are all held in one frame (`and a, $0F / cp a, $0F / jp Init`,
  bank0.asm:1075-1079). Because a throttle-group fire writes `nextAllowed` for every member
  (`decoder.ts:208-209`), Start and Select can never be held simultaneously, so the reset combination
  is structurally unreachable. Assert it in a test.

**Doctrine check.** These are readout parameters, fixed once, identical for every run, carrying no
learning; plasticity remains KC->MBON only. One item needs the operator's explicit sign-off: I put `right`
first in `channels`, and insertion order breaks argmax ties (`decoder.ts:187`, and `readout.md`
"Ties keep the earlier channel"). That is a mild fixed prior toward rightward travel. It should
either be accepted and disclosed on the honesty panel, or the Pokemon order (`up, down, left, right`)
kept for strict neutrality. My recommendation: accept it and disclose it, since a tie is measure-zero
in practice and the label is cheap.

## 5. Recovery and ratchet rules

The Pokemon ratchet (`ratchet.ts`) is reused unchanged in shape: rank-monotone archive, capture on
the first safe sample at a new best rank, stall window, attempt and lifetime budgets.

- **What is archived.** The emulator state plus its exact 160x144x4 framebuffer, at the first *safe*
  (§2) sample whose rank exceeds `state.best`. Nothing below the best rank is ever archived; rank
  regression is rejected server-side as today.
- **Within-level granularity.** None beyond rank 3 (halfway 1-1). Archiving arbitrary mid-level
  positions would mean restoring a stale game timer and a stale enemy table, and would break the
  rank-monotone invariant the archive retention logic depends on.
- **Restore trigger A, game over.** `hGameState` observed as `0x39` or `0x3A`, or `wLives == 0` with
  `wGameOverWindowEnabled` set. Restore immediately, no stall wait: a game over discards the entire
  run and returns to a static title screen. This needs its own, larger budget, because the Pokemon
  limits (3 attempts per rank, 12 lifetime) would be spent within the first hour of a platformer.
  Proposal: 3 per rank, 48 lifetime, 60 s cooldown, disclosed on screen as a counter.
  *Implemented as proposed.* Pokemon's lifetime budget has since become 36 rather than 12
  (`docs/design/ladder.md`, the 38-rung ladder), so the gap is narrower than this paragraph
  assumed, but the argument and the numbers here stand: 48 and the 60 s game-over cooldown are
  `platformer::RECOVERY_POLICY`, and Pokemon keeps `RecoveryPolicy::default()`. Only the budgets
  and triggers are per-game; the ratchet's *rank* bound is the running adapter's ladder length,
  16 here, passed to `Ratchet::import`.
- **Restore trigger B, stall.** No new `band` payout in the current level for 300 brain seconds, and
  at least 180 s since the last recovery, and attempts for this rank < 3. Same shape as Pokemon's
  120 s / 180 s.
- **What resets the stall clock.** Any new band, any rank increase, and sustained unsafe state for
  >= 1 brain second (`ratchet.ts:16-19` semantics unchanged; brief airborne frames must not age into
  a reset, which is why `unsafeSince` uses a 1 s floor).
- **What a restore does.** `agent.resetTransients(frame)`: release every button, clear decoder holds
  and fatigue, clear plastic eligibility, clear the adapter's transient state (stable counter,
  per-life score/coin latches), and refresh visual drive from a *copy* of the archived framebuffer.
  Untouched: brain membranes, RNG, clock, learned KC->MBON gains, and the entire lifetime reward
  ledger. Because the ledger survives, the restored ground pays nothing on the way back.

## 6. Stream differences

- **Milestone panel.** The 16-rung ladder, plus a 4x3 level grid (1-1 .. 4-3) with cleared levels
  filled and the current level ringed. Hero number: `FURTHEST IN 1-2: 63%`, from
  `(hScreenIndex*20 + hColumnIndex - 60) / ((screens[level]-3)*20)`, with `screens[]` the static
  table from §1. A per-level bar with a ghost marker at the lifetime best for that level, and ticks
  at the verified checkpoint screens 3, 7, 11, 15, 19, 23.
- **Stuck-o-meter.** Time since the last new band, on a 5-minute dial (Pokemon uses 2). Beside it:
  `LIVES 3`, `DEATHS THIS LEVEL 14`, and the game timer as a secondary tension meter. Deaths are the
  single most legible "something is happening" signal a platformer gives; show them, and show that
  they are free.
- **Event ticker.** COIN, +200, SUPER MARIO, SUPERBALL, 1UP, NEW GROUND, CHECKPOINT, LEVEL CLEARED,
  WORLD CLEARED, RECOVERY, and `DIED (no penalty, ground kept)`. The parenthetical matters: viewers
  will otherwise assume dying punishes the fly, and the doctrine is that it does not.
- **Honesty panel additions.** "Deaths are not punished. The fly is only paid for ground it has never
  reached." And: "Progress is measured from the camera, not from Mario" (the loader front runs up to
  one screen ahead of the camera; see below).
- **Retina input.** `projectFrame` in `packages/brain/src/model/retina.ts` is nearest-neighbour
  Rec. 709 luminance times `gain = 0.20`, one pixel per column, with no temporal filter, no motion
  detector and no contrast normalisation. A side-scroller changes almost every sampled pixel every
  frame, where Pokemon's maps are static between 16-pixel steps. Two concrete risks:
  1. **Score saturation.** SML's skies are the lightest DMG shade across most of the upper screen, so
     mean drive is higher and more uniform. Decoder scores are ratios against a single `calibrate()`
     taken at rest on a near-black boot screen (`readout.md`, "Score"). If every role's rate sits well
     above its baseline, threshold-1 pulses fire at their cooldown limit forever (A becomes a metronome)
     and the exclusive argmax is decided by noise-scale differences.
  2. **Refractory compression.** If rates instead hit the LIF ceiling, score differences compress and
     the tie-break order (see §4) effectively decides direction.
- **What to measure**, over a 30-minute run, against a matched Pokemon run: distribution of per-column
  drive (mean, p50, p95, fraction at 0, fraction at max); mean and p95 per-role rate for
  `command_0..7`; the eight normalized scores (mean, s.d., fraction above 1.0 / 1.10 / 2.0);
  fraction of `decode()` calls whose argmax margin is below `hysteresis`; per-neuron spike-rate
  histogram versus the Pokemon baseline; total retina input current per brain ms.
- **Decision rule.** If more than ~80% of A-channel samples exceed threshold, raise the A/B
  thresholds (a documented readout parameter) rather than touching `retina.gain`; changing the retina
  changes the model, changing a threshold changes the readout. Either way, record it in the
  compatibility fingerprint.

## 7. Verification plan

**Synthetic traces** (a fake `MemoryReader` over a byte map, one test per rule, mirroring what
`rewards-learning.md` describes for Pokemon):

boot -> playable pays `started` once; band novelty; band oscillation immunity across a boundary; band
cap at `2*(screens-3)`; death resets in-level position, pays nothing, and re-paying the same bands is
impossible; coin BCD increase; **coin BCD wrap `0x99 -> 0x00` counts as +1 and never as a decrease**;
score 3-byte BCD delta with decay and cap; powerup `0 -> 2` pays once per level, `2 -> 3` (injury) and
`3 -> 2` pay nothing; superball `0 -> 1` pays once; 1UP via `wLives` rising; level clear on
`hLevelIndex 0 -> 1` with `hWorldAndLevel 0x11 -> 0x12`; world clear on `0x13 -> 0x21`; game clear on
`hWinCount` rising; attract-demo gate (`0xFF9F != 0` pays nothing); pause gate; autoscroll level is
playable but never safe; game-over states `0x39`/`0x3A`; world/level/index inconsistency reports
`TRANSITION`; mid-level import baselines and pays nothing; invalid checkpoint import throws;
one-read-per-address caching.

**Decoder tests:** right-holds are gapless (`holdMs === decisionMs`); B is a sustained hold and
releases within 600 ms of the score dropping; A fires at most once per 420 ms; Start cannot fire
during play at threshold 2.0 but can in boot; **A, B, Start and Select are never simultaneously
active** (guards the bank0.asm:1075 soft reset); `clearHolds` lockout is 300 ms.

**Ratchet tests:** rank ladder monotone and `rank = 4 + highestClearedLevelIndex`; capture only on
safe samples (on ground, jump status 0, not i-frames, timer > 100, not underground, not autoscroll);
game-over restore path with its own budget; stall restore path; budget exhaustion leaves the run
unrecovered; rank regression rejected; restore preserves brain state, gains and the lifetime ledger.

**ROM-gated integration test**, skipped unless `FLY_ROM_PLATFORMER` is set (mirroring `POKEMON_ROM`):
assert the ROM SHA-256 equals `SUPPORTED_ROM`; boot to playable within N frames with only neural
input; at the first playable sample assert `hWorldAndLevel == 0x11`, `hLevelIndex == 0`,
`hScreenIndex == 3`; adapter mode reads `IN LEVEL 1-1`; 60 brain seconds of play produce at least one
reward event; a seeded-archive variant (a `RATCHET_CHECKPOINT`-equivalent env var) exercises the
restore path without waiting out the real stall.

**30-minute random-walker baseline.** Replace the network's role rates with a seeded uniform random
source, keep the real `platformer` decoder and the real adapter, run 30 brain minutes across 5 seeds
headless. Report: levels reached, furthest band per level, deaths, coins, score, total reward, reward
per brain minute, per-channel active-frame fractions. Calibration target: **0.5 to 2.0 total reward
per brain minute**, dominated by `band` and `coin`, and 1-1 cleared in at most 1 of 5 seeds. If
random clears 1-1 in every seed, coarsen bands to a full screen and halve `coin`; if random earns
under 0.1/min, halve the band width. This calibrates the reward scale only; it is not evidence about
the fly, and the numbers must never be presented as such.

## 8. Open items and risks

1. **Unnamed addresses this design depends on.** `0xFF9F` (attract-demo gate, the most important one:
   without it the demo farms rewards), `0xFFF9` (underground), `0xFFF4`/`0xFFF5` (pipe exit),
   `0xC0AB` (column progress), `0xC0D2` (end-of-level counter), and the Mario physics bytes
   `0xC207`/`0xC20A`/`0xC20C`/`0xC20E`. All are comment-only in the disassembly, so all are
   **UNVERIFIED**. Confirm each with bgb or mGBA watchpoints in a one-day spike before the adapter
   ships.
2. **`hScreenIndex` is the column loader, not Mario.** It advances as `hScrollX` wraps and runs up to
   one screen ahead of the camera (bank0.asm:5215-5223). It is monotone within a level and resets to
   a checkpoint on death, which is exactly the progress signal we want, but it is camera progress and
   must be labelled as such on screen.
3. **Underground aliasing.** Pipe sub-rooms reuse `hScreenIndex`, so underground bands would alias
   onto the main level's bands. Safe v1: suppress `band` payouts and never archive while
   `0xFFF9 != 0`. Revisit once `0xFFF4`/`0xFFF5` are verified.
4. **Jump physics not source-verified.** `GameState_00` is still `INCBIN`-ed from `baserom.gb`
   (bank0.asm:4299), and the README puts bank 0 coverage at three quarters and HRAM at 46 of 127
   bytes identified. The A-hold-to-height curve therefore rests on the manual plus measurement, and
   the 300 ms `holdMs` is a starting estimate to be tuned against the random-walker baseline.
5. **ROM revision drift.** The disassembly pins Super Mario Land (World) (Rev A). Layout on Rev 0 or
   the JP release is **UNVERIFIED**. Pin one SHA-256, fail visibly with `SEMANTIC REWARDS OFF`
   otherwise, and fold the hash plus `sml-progress-v1` plus the preset name into the compatibility
   fingerprint so a checkpoint cannot be loaded under different semantics.
6. **Latency.** At ~16.74 brain ms per frame with 250 ms direction commitments and 300 ms jump holds,
   frame-precise platforming is impossible. Expect the fly to live in world 1 and to clear 1-1 rarely.
   Say so in the stream copy; it is the honest framing, and it is the reason `band` is the primary
   reward rather than `level`.
7. **VRAM is closed off, and that is now a verified fact rather than a worry** (see §1): binjgb's
   public read path returns `0xFF` for VRAM during PPU mode 3. Any future temptation to read the HUD
   should be refused.
8. **IP.** No candidate has a risk advantage: SML, Kirby's Dream Land and Wario Land are all
   Nintendo-published. Exposure is the ROM, not the game, identical to Pokemon Red. Category
   discovery favours Super Mario Land, which is the one real asymmetry and it favours the primary
   choice.
9. **Fallback trigger.** If `0xFF9F` or the underground aliasing cannot be resolved in two days,
   switch to Kirby's Dream Land on the symbols in §1, accepting that its health byte and its playable
   gate both need their own RAM search and that its 5-stage structure compresses the ladder to about
   10 ranks.

### Critical files for implementation

- `packages/brain/src/readout/presets/gameboy.ts` (template for the new `platformer.ts`)
- `packages/brain/src/readout/decoder.ts` (hold/pulse/throttle semantics the preset relies on)
- `~/fly-plays-pokemon/src/reward/pokemon-red.ts` (adapter shape: per-sample byte cache, gates, `once()` ledger, baselining, export/import)
- `~/fly-plays-pokemon/src/reward/catalog.ts` (catalog shape and stimulation-ms field)
- `~/fly-plays-pokemon/src/runtime/ratchet.ts` (rank-monotone archive, stall window, budgets; needs the game-over trigger and the larger budget)
- `packages/brain/src/model/retina.ts` (the luminance projection whose saturation §6 asks you to measure)

Sources: [kaspermeerts/supermarioland](https://github.com/kaspermeerts/supermarioland) ([hram.asm](https://github.com/kaspermeerts/supermarioland/blob/master/hram.asm), [wram.asm](https://github.com/kaspermeerts/supermarioland/blob/master/wram.asm), [bank0.asm](https://github.com/kaspermeerts/supermarioland/blob/master/bank0.asm), [levels/levels.asm](https://github.com/kaspermeerts/supermarioland/blob/master/levels/levels.asm), [rom.sha1](https://github.com/kaspermeerts/supermarioland/blob/master/rom.sha1)) · [huderlem/kirbydreamland wram.asm](https://github.com/huderlem/kirbydreamland/blob/master/wram.asm) · [Kak2X/wl src/memory.asm](https://github.com/Kak2X/wl/blob/master/src/memory.asm) · [binji/binjgb src/emulator.c](https://github.com/binji/binjgb/blob/main/src/emulator.c) · [Data Crystal: Super Mario Land RAM map](https://datacrystal.tcrf.net/wiki/Super_Mario_Land/RAM_map) · [Data Crystal: Kirby's Dream Land RAM map](https://datacrystal.tcrf.net/wiki/Kirby's_Dream_Land:RAM_map) · [Data Crystal: Donkey Kong (Game Boy)](https://datacrystal.tcrf.net/wiki/Donkey_Kong_(Game_Boy)) · [Mario Wiki: Super Mario Land](https://www.mariowiki.com/Super_Mario_Land) · [Mario Wiki: Jump](https://www.mariowiki.com/Jump)
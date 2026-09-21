//! Synthetic WRAM traces for every scene.
//!
//! One test per scene, plus the four the brief calls out specifically: a battle never reads as
//! overworld, a dialog never reads as overworld, an unreadable frame reads as `Unknown`, and
//! standing on a doormat is still the overworld.

use super::detect;
use crate::pokemon_red::fake_wram::{OAKS_LAB, PALLET_TOWN, REDS_HOUSE_1F, Wram};
use crate::pokemon_red::macros::state::Scene;
use crate::pokemon_red::state::poke;
use crate::pokemon_red::symbols::ram;

#[test]
fn a_cartridge_that_has_not_started_is_the_title() {
    let mut wram = Wram::new();
    assert_eq!(detect(&mut wram), Scene::Title);
    // The intro and the naming screens are the same state: a loaded map is not enough.
    wram.map(REDS_HOUSE_1F, 4, 4, 3, 6);
    assert_eq!(detect(&mut wram), Scene::Title);
    // And it is exactly the reward adapter's BOOT gate.
    wram.started();
    assert_ne!(detect(&mut wram), Scene::Title);
}

#[test]
fn a_loaded_map_with_the_buttons_reaching_the_player_is_the_overworld() {
    let mut wram = Wram::overworld();
    assert_eq!(detect(&mut wram), Scene::Overworld);
}

#[test]
fn standing_on_a_door_is_still_the_overworld() {
    // `docs/design/room-escape.md` section 3: the fly spends its life on and next to doormats,
    // and BIT_STANDING_ON_DOOR, BIT_EXITING_DOOR and BIT_STANDING_ON_WARP are all inside the
    // reward adapter's scripted mask. A scene detector that borrowed that mask whole would
    // report Unknown on the one tile that matters most.
    for bits in [0b001, 0b010, 0b100, 0b111] {
        let mut wram = Wram::overworld();
        wram.set(ram::wMovementFlags, bits);
        assert_eq!(detect(&mut wram), Scene::Overworld, "wMovementFlags {bits:#04b}");
    }
    // A ledge hop and a spin tile are not the fly's to act on.
    for bits in [0b0100_0000, 0b1000_0000] {
        let mut wram = Wram::overworld();
        wram.set(ram::wMovementFlags, bits);
        assert_eq!(detect(&mut wram), Scene::Unknown, "wMovementFlags {bits:#010b}");
    }
}

#[test]
fn a_scripted_frame_is_not_the_overworld() {
    let cases: [(&str, u16, u8); 5] = [
        ("wJoyIgnore", ram::wJoyIgnore, 0xff),
        ("wSimulatedJoypadStatesIndex", ram::wSimulatedJoypadStatesIndex, 3),
        ("wStatusFlags5 scripted movement", ram::wStatusFlags5, 0x80),
        ("wStatusFlags5 joypad disabled", ram::wStatusFlags5, 0x20),
        ("wStatusFlags6 fly warp", ram::wStatusFlags6, poke::BIT_GAME_TIMER_COUNTING | 0x08),
    ];
    for (name, address, value) in cases {
        let mut wram = Wram::overworld();
        wram.set(address, value);
        assert_eq!(detect(&mut wram), Scene::Unknown, "{name}");
    }
}

#[test]
fn a_frame_whose_map_header_is_not_loaded_is_unknown() {
    let mut wram = Wram::overworld();
    wram.set(ram::wCurMapWidth, 0);
    assert_eq!(detect(&mut wram), Scene::Unknown);

    let mut wram = Wram::overworld();
    wram.set(ram::wCurMap, 0xff);
    assert_eq!(detect(&mut wram), Scene::Unknown);

    // Coordinates outside the map: a mid-warp frame.
    let mut wram = Wram::overworld();
    wram.set(ram::wXCoord, 40);
    assert_eq!(detect(&mut wram), Scene::Unknown);

    // A party count the game cannot produce.
    let mut wram = Wram::overworld();
    wram.set(ram::wPartyCount, 7);
    assert_eq!(detect(&mut wram), Scene::Unknown);
}

#[test]
fn an_open_dialogue_box_is_a_dialog() {
    let mut wram = Wram::overworld();
    wram.dialogue_box();
    assert_eq!(detect(&mut wram), Scene::Dialog);
}

#[test]
fn a_dialog_never_reads_as_the_overworld() {
    // Everything about the frame says "walkable overworld" except the one thing that matters:
    // the font is loaded and the box is drawn, so the player cannot move at all
    // (`tests/rom.rs` measured 3,000 frames of directions that never left (3, 6)).
    let mut wram = Wram::overworld();
    wram.set(ram::wCurMap, OAKS_LAB).dialogue_box();
    assert_ne!(detect(&mut wram), Scene::Overworld);
    assert_eq!(detect(&mut wram), Scene::Dialog);

    // The font flag alone, with no box the detector recognises, must not fall through to the
    // overworld either.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED);
    assert_eq!(detect(&mut wram), Scene::Unknown);
}

#[test]
fn the_start_menu_is_a_menu() {
    let mut wram = Wram::overworld();
    wram.start_menu();
    assert_eq!(detect(&mut wram), Scene::Menu);

    // Without the Pokédex the box is two rows shorter and there is one item fewer.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .draw_box(10, 0, 19, 13)
        .cursor(2, 11, 0, 6, poke::pad::A);
    assert_eq!(detect(&mut wram), Scene::Menu);
}

#[test]
fn a_submenu_is_a_menu() {
    // The bag list, from the start menu's ITEM entry.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .set(ram::wListMenuID, poke::ITEM_LIST_MENU);
    assert_eq!(detect(&mut wram), Scene::Menu);

    // An elevator's floor list.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .set(ram::wListMenuID, poke::SPECIAL_LIST_MENU);
    assert_eq!(detect(&mut wram), Scene::Menu);

    // The party list outside a battle, from the POKEMON entry.
    let mut wram = Wram::overworld();
    wram.party_mon(0, 4, 5, 19, 19, 0, &[(33, 35)]);
    wram.party_list(0, false);
    assert_eq!(detect(&mut wram), Scene::Menu);
}

#[test]
fn the_cursor_geometry_alone_does_not_open_a_menu() {
    // HandleMenuInput's state survives the menu closing, so the start menu's geometry is still
    // in WRAM while the fly walks around. Only the drawn box plus the font flag is a menu.
    let mut wram = Wram::overworld();
    wram.cursor(2, 11, 0, 7, poke::pad::A);
    assert_eq!(detect(&mut wram), Scene::Overworld);
}

#[test]
fn a_wild_battle_with_the_menu_up_is_the_flys_turn() {
    let mut wram = battle_frame(1);
    wram.battle_menu(false, 0);
    assert_eq!(detect(&mut wram), Scene::Battle { own_turn: true, forced_switch: false });
}

#[test]
fn a_battle_with_no_menu_up_is_not_the_flys_turn() {
    let mut wram = battle_frame(2);
    assert_eq!(detect(&mut wram), Scene::Battle { own_turn: false, forced_switch: false });
}

#[test]
fn the_move_list_open_is_the_flys_turn() {
    // Live 2026-09-17, an hour and forty-one minutes in Viridian Forest: the move list was open
    // with TACKLE at 0 PP, and `own_turn` was `Main` alone — so the frame read as "between turns",
    // whose pad is one `NEXT`, an A press on whatever the cursor is sitting on. It pressed A at the
    // move with no PP, the game said so, the box closed, the list came back, every 268 brain
    // milliseconds. A list that is accepting input is the game waiting for the player to choose,
    // which is what a turn is.
    let mut wram = battle_frame(1);
    wram.battle_menu(false, 0).move_menu(2, 4);
    assert_eq!(detect(&mut wram), Scene::Battle { own_turn: true, forced_switch: false });
}

#[test]
fn the_party_list_a_fainted_mon_forces_is_a_forced_switch() {
    let mut wram = battle_frame(2);
    wram.party_mon(1, 16, 8, 24, 24, 0, &[(33, 35)]);
    wram.party_list(0, true);
    assert_eq!(detect(&mut wram), Scene::Battle { own_turn: false, forced_switch: true });

    // The same list opened from the menu's PKMN entry is not forced: it sets NORMAL_PARTY_MENU
    // and it watches B, so the player can back out — and it is an ordinary part of the fly's turn,
    // because the game is waiting for it to choose.
    let mut wram = battle_frame(2);
    wram.party_mon(1, 16, 8, 24, 24, 0, &[(33, 35)]);
    wram.party_list(0, false);
    assert_eq!(detect(&mut wram), Scene::Battle { own_turn: true, forced_switch: false });
}

#[test]
fn a_battle_never_reads_as_the_overworld_or_a_dialog() {
    // A battle is made of text boxes: the font is loaded, the dialogue box is drawn, and the
    // map underneath is still the one the fly was walking on. It is a battle.
    let mut wram = battle_frame(1);
    wram.dialogue_box();
    assert_eq!(detect(&mut wram), Scene::Battle { own_turn: false, forced_switch: false });

    // Even with every overworld gate open.
    let mut wram = Wram::overworld();
    wram.set(ram::wIsInBattle, 1).enemy_mon(19, 3, 11, 11);
    assert!(matches!(detect(&mut wram), Scene::Battle { .. }));
}

#[test]
fn a_battle_the_detector_does_not_understand_is_unknown() {
    // The frame a lost battle passes through.
    let mut wram = battle_frame(0xff);
    assert_eq!(detect(&mut wram), Scene::Unknown);

    // The Safari Zone and the old man's tutorial have their own menus.
    for battle_type in [1, 2] {
        let mut wram = battle_frame(1);
        wram.set(ram::wBattleType, battle_type);
        assert_eq!(detect(&mut wram), Scene::Unknown, "wBattleType {battle_type}");
    }
}

#[test]
fn a_mart_is_a_shop() {
    // The BUY / SELL / QUIT choice.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .set(ram::wTextBoxID, poke::BUY_SELL_QUIT_MENU);
    assert_eq!(detect(&mut wram), Scene::Shop);

    // The priced buy list, which no other screen in the game uses.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .set(ram::wListMenuID, poke::PRICED_ITEM_LIST_MENU);
    assert_eq!(detect(&mut wram), Scene::Shop);

    // The mart's greeting is still a dialog: the box is drawn and neither mart signal is set yet.
    let mut wram = Wram::overworld();
    wram.dialogue_box();
    assert_eq!(detect(&mut wram), Scene::Dialog);
}

#[test]
fn a_pc_is_a_pc() {
    let mut wram = Wram::overworld();
    wram.set(ram::wMiscFlags, poke::BIT_USING_GENERIC_PC);
    assert_eq!(detect(&mut wram), Scene::Pc);

    // It outranks the text box the PC prints into, because a PC's dialogue is a PC's.
    let mut wram = Wram::overworld();
    wram.set(ram::wMiscFlags, poke::BIT_USING_GENERIC_PC).dialogue_box();
    assert_eq!(detect(&mut wram), Scene::Pc);

    // And a battle outranks a stale PC bit, which is what makes the order in `detect` a contract.
    let mut wram = battle_frame(1);
    wram.set(ram::wMiscFlags, poke::BIT_USING_GENERIC_PC);
    assert!(matches!(detect(&mut wram), Scene::Battle { .. }));
}

#[test]
fn every_scene_the_enum_declares_has_a_synthetic_trace() {
    let mut title = Wram::new();
    let mut overworld = Wram::overworld();
    let mut dialog = Wram::overworld();
    dialog.dialogue_box();
    let mut menu = Wram::overworld();
    menu.start_menu();
    let mut turn = battle_frame(1);
    turn.battle_menu(false, 0);
    let mut switch = battle_frame(2);
    switch.party_mon(1, 16, 8, 24, 24, 0, &[(33, 35)]);
    switch.party_list(0, true);
    let mut shop = Wram::overworld();
    shop.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .set(ram::wTextBoxID, poke::BUY_SELL_QUIT_MENU);
    let mut pc = Wram::overworld();
    pc.set(ram::wMiscFlags, poke::BIT_USING_GENERIC_PC);
    let mut unknown = Wram::overworld();
    unknown.set(ram::wCurMapHeight, 0);

    let observed = [
        detect(&mut title),
        detect(&mut overworld),
        detect(&mut dialog),
        detect(&mut menu),
        detect(&mut turn),
        detect(&mut switch),
        detect(&mut shop),
        detect(&mut pc),
        detect(&mut unknown),
    ];
    assert_eq!(
        observed,
        [
            Scene::Title,
            Scene::Overworld,
            Scene::Dialog,
            Scene::Menu,
            Scene::Battle { own_turn: true, forced_switch: false },
            Scene::Battle { own_turn: false, forced_switch: true },
            Scene::Shop,
            Scene::Pc,
            Scene::Unknown,
        ]
    );
}

#[test]
fn a_scene_labels_itself_for_the_feed() {
    assert_eq!(Scene::Overworld.label(), "OVERWORLD");
    assert_eq!(Scene::Battle { own_turn: true, forced_switch: false }.label(), "BATTLE TURN");
    assert_eq!(Scene::Battle { own_turn: false, forced_switch: true }.label(), "BATTLE SWITCH");
    assert_eq!(Scene::Battle { own_turn: false, forced_switch: false }.label(), "BATTLE");
    assert!(!Scene::Title.playable());
    assert!(Scene::Unknown.playable());
    // Every label fits the palette strip's cell width.
    for scene in [
        Scene::Title,
        Scene::Overworld,
        Scene::Dialog,
        Scene::Menu,
        Scene::Battle { own_turn: true, forced_switch: false },
        Scene::Shop,
        Scene::Pc,
        Scene::Unknown,
    ] {
        assert!(scene.label().len() <= 14, "{}", scene.label());
    }
}

#[test]
fn the_diagnostic_line_reports_something_for_every_frame() {
    let mut wram = Wram::overworld();
    let line = super::why_unknown(&mut wram);
    assert!(line.contains("started=true"), "{line}");
    assert!(line.contains(&format!("map={:?}", Some((8u8, 8u8)))), "{line}");

    let mut title = Wram::new();
    assert!(super::why_unknown(&mut title).contains("started=false"));
}

#[test]
fn the_scene_survives_a_map_the_ladder_cares_about() {
    for map in [REDS_HOUSE_1F, PALLET_TOWN, OAKS_LAB] {
        let mut wram = Wram::overworld();
        wram.set(ram::wCurMap, map);
        assert_eq!(detect(&mut wram), Scene::Overworld, "map {map:#04x}");
    }
}

/// A battle frame: started, a loaded map, an enemy, and `wIsInBattle` set.
fn battle_frame(is_in_battle: u8) -> Wram {
    let mut wram = Wram::overworld();
    wram.party_mon(0, 4, 5, 19, 19, 0, &[(33, 35), (10, 35)])
        .battle_mon(0, 4, 5, 19, 19, 0, &[(33, 35), (10, 35)])
        .enemy_mon(19, 3, 11, 11)
        .battle(is_in_battle);
    wram
}

#[test]
fn four_map_tiles_that_look_like_a_box_are_not_a_dialog() {
    // The hazard the 2026-09-17 residual was investigated for, and the one thing that turned out
    // to be wrong (`infra/docs/macros-traps.md`). The dialogue box's `waiting` test used to read
    // four screen tiles — the corners of a box at (0, 12)-(19, 17) — and the frame's tile ids
    // ($79, $7b, $7d, $7e) are ordinary ground in the overworld tilesets: on the cartridge, from
    // the stalled checkpoint, **315 frames of 43,004** had all four of those positions holding
    // them with no text box anywhere. `wFontLoaded` was clear on every one of those frames, so
    // nothing came of it — but the gate was the only thing standing between a map and a scene.
    let mut wram = Wram::overworld();
    wram.draw_box_corners(0, 12, 19, 17);
    assert_eq!(detect(&mut wram), Scene::Overworld, "the font flag is what makes a box a box");

    // And with the flag set for a text display that is *not* the dialogue box — which is what a
    // submenu the detector does not recognise looks like — four corners must not promote it.
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED);
    assert_eq!(
        detect(&mut wram),
        Scene::Unknown,
        "an unrecognised text display stays Unknown, which the doctrine advances only"
    );

    // The whole `TextBoxBorder`, which is what the game actually draws, is a dialog.
    let mut real = Wram::overworld();
    real.dialogue_box();
    assert_eq!(detect(&mut real), Scene::Dialog);

    // One byte of the border missing is enough to tell the two apart: a map cannot draw
    // seventy-six of them by accident, and the test now reads all seventy-six.
    let mut chipped = Wram::overworld();
    chipped.dialogue_box().screen_tile(9, 12, 0x23);
    assert_eq!(chipped.peek(ram::wFontLoaded), poke::BIT_FONT_LOADED);
    assert_eq!(detect(&mut chipped), Scene::Unknown, "a border with a hole in it is not a box");
}

#[test]
fn the_start_menus_box_is_read_the_same_way() {
    // The same four-corner reading was in `start_menu`, with the same fix: the cursor position and
    // the item count are WRAM and stay the gate, and the drawn half is the whole border.
    let mut wram = Wram::overworld();
    wram.start_menu();
    assert_eq!(detect(&mut wram), Scene::Menu);

    let mut corners = Wram::overworld();
    corners
        .set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .draw_box_corners(10, 0, 19, 15)
        .cursor(2, 11, 0, 7, poke::pad::DOWN | poke::pad::UP | poke::pad::START);
    assert_eq!(detect(&mut corners), Scene::Unknown, "four corners are not the start menu");
}

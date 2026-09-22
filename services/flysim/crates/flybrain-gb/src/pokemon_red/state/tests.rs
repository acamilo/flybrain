//! Synthetic WRAM traces for every accessor.
//!
//! The encodings under test are the ones `docs/design/macros-wram.md` records: big-endian HP,
//! packed PP, a BCD wallet, sprite coordinates biased by four, a one-based move list, and a
//! passable-tile list walked out of ROM bank 0.

use super::*;
use crate::pokemon_red::fake_wram::{self, REDS_HOUSE_1F, WALL_TILE, Wram};
use crate::pokemon_red::macros::cartridge::MacroState;
use crate::pokemon_red::macros::state::{BattleKind, BattleMenu, ShopScreen, Sign};

/// A walkable tile id from `RedsHouse1_Coll`.
const FLOOR: u8 = 0x01;

#[test]
fn the_player_reads_its_map_tile_and_facing() {
    let mut wram = Wram::overworld();
    let here = player(&mut wram).expect("a loaded map");
    assert_eq!((here.map, here.x, here.y), (REDS_HOUSE_1F, 3, 6));
    assert_eq!(here.facing, Facing::Down);

    for (byte, facing) in [
        (0x00, Facing::Down),
        (0x04, Facing::Up),
        (0x08, Facing::Left),
        (0x0c, Facing::Right),
    ] {
        wram.facing(byte);
        assert_eq!(player(&mut wram).unwrap().facing, facing, "sprite facing {byte:#04x}");
        assert_eq!(facing.delta(), match facing {
            Facing::Down => (0, 1),
            Facing::Up => (0, -1),
            Facing::Left => (-1, 0),
            Facing::Right => (1, 0),
        });
    }
}

#[test]
fn a_map_is_measured_in_tiles_not_blocks() {
    let mut wram = Wram::overworld();
    assert_eq!(map_size(&mut wram), Some(MapSize { width: 8, height: 8 }));

    // Pallet Town is 10 by 9 blocks.
    wram.map(0x00, 10, 9, 5, 6);
    assert_eq!(map_size(&mut wram), Some(MapSize { width: 20, height: 18 }));

    // An unloaded header is no size at all, rather than a zero-sized map.
    wram.set(ram::wCurMapWidth, 0);
    assert_eq!(map_size(&mut wram), None);
    assert_eq!(player(&mut wram), None);
}

#[test]
fn the_player_is_none_when_its_coordinates_are_outside_the_map() {
    let mut wram = Wram::overworld();
    wram.set(ram::wYCoord, 8);
    assert_eq!(player(&mut wram), None);
}

#[test]
fn a_party_member_reads_species_level_hp_status_and_moves() {
    let mut wram = Wram::overworld();
    // Charmander, level 7, 14 of 22 HP, asleep for 3 turns, SCRATCH with 34 PP and one PP Up,
    // GROWL with 40.
    wram.party_mon(0, 4, 7, 14, 22, 0b011, &[(10, 0b01_100010), (45, 40)]);

    let team = party(&mut wram);
    assert_eq!(team.mons.len(), 1);
    let mon = team.mons[0];
    assert_eq!(mon.slot, 0);
    assert_eq!(mon.species, 4);
    assert_eq!(mon.level, 7);
    assert_eq!(mon.hp, 14);
    assert_eq!(mon.max_hp, 22);
    assert_eq!(mon.status, Status::Sleep(3));
    assert_eq!(mon.moves[0], Some(Move { id: 10, pp: 34, pp_up: 1 }));
    assert_eq!(mon.moves[1], Some(Move { id: 45, pp: 40, pp_up: 0 }));
    assert_eq!(mon.moves[2], None);
    assert_eq!(mon.moves[3], None);
    assert!(!mon.fainted());
    assert!((mon.hp_fraction() - 14.0 / 22.0).abs() < 1e-12);

    // Outside a battle there is no active Pokémon.
    assert_eq!(team.active, None);
}

#[test]
fn hp_is_big_endian() {
    let mut wram = Wram::overworld();
    // 0x0102 = 258: a byte-swapped read would say 513.
    wram.party_mon(0, 6, 36, 258, 300, 0, &[]);
    assert_eq!(party(&mut wram).mons[0].hp, 258);
    assert_eq!(wram.peek(ram::wPartyMon1 + 1), 0x01);
    assert_eq!(wram.peek(ram::wPartyMon1 + 2), 0x02);
}

#[test]
fn every_status_byte_reads_as_its_ailment() {
    let mut wram = Wram::overworld();
    for (byte, status) in [
        (0x00, Status::Healthy),
        (0x01, Status::Sleep(1)),
        (0x07, Status::Sleep(7)),
        (1 << 3, Status::Poison),
        (1 << 4, Status::Burn),
        (1 << 5, Status::Freeze),
        (1 << 6, Status::Paralysis),
    ] {
        wram.party_mon(0, 4, 5, 19, 19, byte, &[]);
        assert_eq!(party(&mut wram).mons[0].status, status, "status byte {byte:#04x}");
    }
}

#[test]
fn a_full_party_reads_six_members_in_slot_order() {
    let mut wram = Wram::overworld();
    for slot in 0..6u8 {
        wram.party_mon(slot, 10 + slot, 5 + slot, u16::from(slot) + 1, 20, 0, &[(33, 35)]);
    }
    let team = party(&mut wram);
    assert_eq!(team.mons.len(), 6);
    for slot in 0..6usize {
        assert_eq!(team.mons[slot].slot, slot as u8);
        assert_eq!(team.mons[slot].species, 10 + slot as u8);
    }
    // A count the cartridge cannot produce reads as no party at all, not as garbage.
    wram.set(ram::wPartyCount, 9);
    assert!(party(&mut wram).mons.is_empty());
}

#[test]
fn the_healthiest_reserve_is_the_highest_fraction_that_is_not_out_and_not_fainted() {
    let mut wram = Wram::overworld();
    wram.party_mon(0, 4, 10, 30, 30, 0, &[]); // out, full
    wram.party_mon(1, 7, 10, 0, 30, 0, &[]); // fainted
    wram.party_mon(2, 16, 10, 10, 40, 0, &[]); // 0.25
    wram.party_mon(3, 19, 10, 15, 30, 0, &[]); // 0.50
    wram.set(ram::wIsInBattle, 1).set(ram::wPlayerMonNumber, 0);

    let team = party(&mut wram);
    assert_eq!(team.active, Some(0));
    assert_eq!(team.active_mon().map(|mon| mon.species), Some(4));
    assert_eq!(team.healthiest_reserve().map(|mon| mon.slot), Some(3));

    // Ties go to the lower slot, which is `macros.md` section 3's "ties by index".
    let mut wram = Wram::overworld();
    wram.party_mon(0, 4, 10, 30, 30, 0, &[]);
    wram.party_mon(1, 7, 10, 20, 40, 0, &[]);
    wram.party_mon(2, 16, 10, 10, 20, 0, &[]);
    wram.set(ram::wIsInBattle, 1).set(ram::wPlayerMonNumber, 0);
    assert_eq!(party(&mut wram).healthiest_reserve().map(|mon| mon.slot), Some(1));

    // A party of one has no reserve.
    let mut wram = Wram::overworld();
    wram.party_mon(0, 4, 10, 30, 30, 0, &[]);
    wram.set(ram::wIsInBattle, 1);
    assert!(party(&mut wram).healthiest_reserve().is_none());
}

#[test]
fn a_battle_reports_its_kind_its_own_mon_and_the_enemy() {
    let mut wram = Wram::overworld();
    wram.party_mon(0, 4, 7, 14, 22, 0, &[(10, 35)])
        .battle_mon(0, 4, 7, 14, 22, 0, &[(10, 35), (45, 40)])
        .enemy_mon(19, 3, 5, 11)
        .battle(1);

    let fight = battle(&mut wram).expect("a wild battle");
    assert_eq!(fight.kind, BattleKind::Wild);
    assert_eq!(fight.own.map(|mon| (mon.species, mon.hp, mon.max_hp)), Some((4, 14, 22)));
    assert_eq!(fight.own.unwrap().moves[1], Some(Move { id: 45, pp: 40, pp_up: 0 }));
    assert_eq!(
        fight.enemy,
        Some(EnemyMon { species: 19, level: 3, hp: 5, max_hp: 11 })
    );
    assert_eq!(fight.menu, BattleMenu::None);
    assert!(!fight.own_turn);

    wram.set(ram::wIsInBattle, 2);
    assert_eq!(battle(&mut wram).unwrap().kind, BattleKind::Trainer);

    // Outside a battle there is nothing to report.
    wram.set(ram::wIsInBattle, 0);
    assert!(battle(&mut wram).is_none());
}

#[test]
fn the_battle_menu_cursor_maps_to_fight_pkmn_item_run() {
    for (right_column, current, expected) in
        [(false, 0, 0u8), (false, 1, 1), (true, 0, 2), (true, 1, 3)]
    {
        let mut wram = Wram::overworld();
        wram.battle_mon(0, 4, 7, 14, 22, 0, &[(10, 35)]).battle(1).battle_menu(
            right_column,
            current,
        );
        let fight = battle(&mut wram).unwrap();
        assert_eq!(
            fight.menu,
            BattleMenu::Main { cursor: expected },
            "right={right_column} current={current}"
        );
        assert!(fight.own_turn);
        assert!(!fight.forced_switch);
    }
}

#[test]
fn the_move_list_is_reported_zero_based() {
    let mut wram = Wram::overworld();
    wram.battle_mon(0, 4, 7, 14, 22, 0, &[(10, 35), (45, 40), (33, 30)]).battle(1);

    for slot in 0..3u8 {
        wram.move_menu(slot, 3);
        assert_eq!(
            battle(&mut wram).unwrap().menu,
            BattleMenu::Moves { cursor: Some(slot), count: 3 },
            "move slot {slot}"
        );
    }

    // The game's list is one-based, so index 0 names no move and neither does one past the end.
    wram.move_menu(0, 3).set(ram::wCurrentMenuItem, 0);
    assert_eq!(battle(&mut wram).unwrap().menu, BattleMenu::Moves { cursor: None, count: 3 });
    wram.move_menu(0, 3).set(ram::wCurrentMenuItem, 4);
    assert_eq!(battle(&mut wram).unwrap().menu, BattleMenu::Moves { cursor: None, count: 3 });
}

#[test]
fn a_forced_switch_is_the_party_list_that_cannot_be_cancelled() {
    let mut wram = Wram::overworld();
    wram.party_mon(0, 4, 7, 0, 22, 0, &[(10, 35)]);
    wram.party_mon(1, 16, 8, 24, 24, 0, &[(33, 35)]);
    wram.battle_mon(0, 4, 7, 0, 22, 0, &[(10, 35)]).battle(2).party_list(1, true);

    let fight = battle(&mut wram).unwrap();
    assert_eq!(fight.menu, BattleMenu::Party { cursor: 1 });
    assert!(fight.forced_switch);
    assert!(!fight.own_turn);

    let mut wram = Wram::overworld();
    wram.party_mon(0, 4, 7, 14, 22, 0, &[(10, 35)]);
    wram.party_mon(1, 16, 8, 24, 24, 0, &[(33, 35)]);
    wram.battle_mon(0, 4, 7, 14, 22, 0, &[(10, 35)]).battle(2).party_list(1, false);
    let fight = battle(&mut wram).unwrap();
    assert_eq!(fight.menu, BattleMenu::Party { cursor: 1 });
    assert!(!fight.forced_switch);
    assert!(cursor(&mut wram).cancellable());
}

/// Section 12.10: **a battle frame with a cursor accepting input is the fly's turn.**
///
/// The four menus a battle waits on are the top-level one, the move list, the party list and the
/// bag, and each of them is the game asking the player to choose. `own_turn` answered `false` for
/// the bag, which put it on the between-turns row whose one button is the `NEXT` that advances
/// text -- and on an open bag that same A press uses whatever the cursor holds.
///
/// The two frames that are correctly *not* the fly's turn are here too: a move list whose cursor
/// the seam cannot place (row 30b, a battle's opening frames) and a frame with no menu at all.
#[test]
fn a_battle_frame_with_a_cursor_accepting_input_is_the_flys_turn() {
    let battler = |wram: &mut Wram| {
        wram.party_mon(0, 4, 7, 14, 22, 0, &[(10, 35)]);
        wram.party_mon(1, 16, 8, 24, 24, 0, &[(33, 35)]);
        wram.battle_mon(0, 4, 7, 14, 22, 0, &[(10, 35), (45, 40), (33, 30)])
            .enemy_mon(19, 3, 5, 11)
            .battle(1);
    };

    // The top-level menu.
    let mut wram = Wram::overworld();
    battler(&mut wram);
    wram.battle_menu(false, 0);
    assert!(battle(&mut wram).unwrap().own_turn, "the top-level menu");

    // The move list, with a cursor the seam can place.
    let mut wram = Wram::overworld();
    battler(&mut wram);
    wram.move_menu(1, 3);
    assert!(battle(&mut wram).unwrap().own_turn, "the move list");

    // The party list, chosen rather than forced.
    let mut wram = Wram::overworld();
    battler(&mut wram);
    wram.party_list(1, false);
    let fight = battle(&mut wram).unwrap();
    assert!(!fight.forced_switch);
    assert!(fight.own_turn, "the party list outside a forced switch");

    // The bag, which `DisplayListMenuID` opens from the menu's ITEM entry. It is the one menu that
    // read as nobody's turn, and it is what section 12.10 is about.
    let mut wram = Wram::overworld();
    battler(&mut wram);
    wram.bag(&[(crate::pokemon_red::macros::cartridge::item::POTION, 2)]).set(ram::wListMenuID, poke::ITEM_LIST_MENU);
    let fight = battle(&mut wram).unwrap();
    assert_eq!(fight.menu, BattleMenu::Bag { cursor: 0, count: 1 });
    assert!(fight.own_turn, "the battle bag is a cursor accepting input");

    // And the two frames that are not a choice. A move list whose cursor cannot be placed is a
    // battle's opening frames (row 30b), and no menu at all is text, an animation or a turn
    // resolving.
    let mut wram = Wram::overworld();
    battler(&mut wram);
    wram.move_menu(0, 3).set(ram::wCurrentMenuItem, 0);
    let fight = battle(&mut wram).unwrap();
    assert_eq!(fight.menu, BattleMenu::Moves { cursor: None, count: 3 });
    assert!(!fight.own_turn, "a cursor the seam cannot place is not accepting input");

    let mut wram = Wram::overworld();
    battler(&mut wram);
    assert_eq!(battle(&mut wram).unwrap().menu, BattleMenu::None);
    assert!(!battle(&mut wram).unwrap().own_turn, "no menu, no turn");

    // A forced switch is a cursor accepting input and it is *not* the own turn, because it has a
    // pad of its own: the exception 12.6 named, kept here so the invariant reads honestly.
    let mut wram = Wram::overworld();
    battler(&mut wram);
    wram.party_list(1, true);
    let fight = battle(&mut wram).unwrap();
    assert!(fight.forced_switch);
    assert!(!fight.own_turn, "a forced switch has its own pad");
}

#[test]
fn a_text_box_is_open_from_the_font_flag_and_waiting_from_the_box() {
    let mut wram = Wram::overworld();
    assert_eq!(text_box(&mut wram), TextBox { open: false, waiting: false });

    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED);
    assert_eq!(text_box(&mut wram), TextBox { open: true, waiting: false });

    wram.dialogue_box();
    assert_eq!(text_box(&mut wram), TextBox { open: true, waiting: true });

    // One stray frame tile on the map is not a text box; four corners are.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .screen_tile(0, 12, poke::frame::TOP_LEFT);
    assert!(!text_box(&mut wram).waiting);
}

#[test]
fn the_start_menu_counts_its_items() {
    let mut wram = Wram::overworld();
    wram.start_menu();
    let menu = start_menu(&mut wram).expect("the start menu");
    assert_eq!(menu.items, 7, "the Pokédex entry is there");
    assert_eq!(menu.cursor.top_x, 11);
    assert!(menu.cursor.cancellable());

    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .draw_box(10, 0, 19, 13)
        .cursor(2, 11, 0, 6, poke::pad::A | poke::pad::B);
    assert_eq!(start_menu(&mut wram).unwrap().items, 6);

    // The box without the font flag is a repaint, not a menu.
    let mut wram = Wram::overworld();
    wram.draw_box(10, 0, 19, 15).cursor(2, 11, 0, 7, poke::pad::A);
    assert!(start_menu(&mut wram).is_none());
}

#[test]
fn a_mart_reports_which_of_its_screens_is_up() {
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .set(ram::wTextBoxID, poke::BUY_SELL_QUIT_MENU);
    assert_eq!(shop(&mut wram).map(|shop| shop.screen), Some(ShopScreen::BuySellQuit));

    wram.set(ram::wListMenuID, poke::ITEM_LIST_MENU);
    assert_eq!(shop(&mut wram).map(|shop| shop.screen), Some(ShopScreen::Selling));

    // The priced list byte with the item window drawn is the buy list.
    wram.set(ram::wListMenuID, poke::PRICED_ITEM_LIST_MENU)
        .screen_tile(poke::MART_NAME_COLUMN, poke::MART_NAME_ROW, poke::CHAR_UPPER_A);
    assert_eq!(shop(&mut wram).map(|shop| shop.screen), Some(ShopScreen::Buying));

    // The bag list on its own is the start menu's, not a mart's.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .set(ram::wListMenuID, poke::ITEM_LIST_MENU);
    assert!(shop(&mut wram).is_none());
}

/// Row 55: the byte that says "the priced buy list" outlives the list, so which screen is up is
/// read from what is drawn.
///
/// Surveyed in the Pewter mart from the live checkpoint: `wListMenuID` held
/// `PRICEDITEMLISTMENU` on **every frame of the visit** -- the counter menu and the clerk's own
/// text boxes included -- because the mart prints its text from inside
/// `DisplayPokemartDialogue_` rather than through `DisplayTextIDInit`. The two things that do
/// change are the figure on screen: the full-width dialogue box for the clerk, and the item
/// window for the list.
#[test]
fn the_clerks_text_box_is_not_the_marts_buy_list() {
    // The frame the stream looped on: the counter open, the priced-list byte stale, and the
    // clerk's "Here you are! Thank you!" waiting for a press.
    let mut wram = Wram::overworld();
    wram.set(ram::wListMenuID, poke::PRICED_ITEM_LIST_MENU)
        .screen_tile(poke::MART_NAME_COLUMN, poke::MART_NAME_ROW, poke::CHAR_UPPER_A)
        .dialogue_box();
    assert_eq!(
        shop(&mut wram).map(|shop| shop.screen),
        Some(ShopScreen::Talking),
        "a dialogue box waiting for a press is the clerk, whatever the list byte says"
    );

    // The same counter with the box gone and the item window drawn: the buy list.
    let mut wram = Wram::overworld();
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .set(ram::wListMenuID, poke::PRICED_ITEM_LIST_MENU)
        .screen_tile(poke::MART_NAME_COLUMN, poke::MART_NAME_ROW, poke::CHAR_UPPER_A);
    assert_eq!(shop(&mut wram).map(|shop| shop.screen), Some(ShopScreen::Buying));

    // And with the item window blank: the BUY / SELL / QUIT menu underneath it.
    wram.screen_tile(poke::MART_NAME_COLUMN, poke::MART_NAME_ROW, poke::frame::HORIZONTAL);
    assert_eq!(
        shop(&mut wram).map(|shop| shop.screen),
        Some(ShopScreen::BuySellQuit),
        "no item name in the window means the list is not the thing on screen"
    );
}

#[test]
fn a_pc_is_the_generic_pc_bit() {
    let mut wram = Wram::overworld();
    assert!(pc(&mut wram).is_none());
    wram.set(ram::wMiscFlags, poke::BIT_USING_GENERIC_PC).cursor(2, 4, 1, 3, poke::pad::A);
    assert_eq!(pc(&mut wram).map(|pc| pc.cursor.current), Some(1));
    // Another flag in the same byte is not a PC.
    wram.set(ram::wMiscFlags, 1 << 4);
    assert!(pc(&mut wram).is_none());
}

#[test]
fn money_is_three_bytes_of_bcd() {
    let mut wram = Wram::overworld();
    assert_eq!(money(&mut wram), 0);

    for amount in [1u32, 9, 10, 99, 100, 3000, 12_345, 999_999] {
        wram.money(amount);
        assert_eq!(money(&mut wram), amount, "{amount}");
    }

    // The starting wallet, written the way the cartridge writes it.
    wram.set(ram::wPlayerMoney, 0x00).set(ram::wPlayerMoney + 1, 0x30).set(
        ram::wPlayerMoney + 2,
        0x00,
    );
    assert_eq!(money(&mut wram), 3000);

    // A nibble above 9 is not BCD: a half-written frame must not read as a plausible number.
    wram.set(ram::wPlayerMoney + 1, 0xfa);
    assert_eq!(money(&mut wram), 0);
}

#[test]
fn the_bag_reads_pairs_up_to_its_terminator() {
    let mut wram = Wram::overworld();
    assert!(bag(&mut wram).is_empty());

    wram.bag(&[(4, 1), (20, 3)]);
    assert_eq!(
        bag(&mut wram),
        vec![BagItem { id: 4, count: 1 }, BagItem { id: 20, count: 3 }]
    );

    // The terminator wins over the count, which is what stops a stale slot being read as an item.
    wram.set(ram::wNumBagItems, 4).set(ram::wBagItems + 2, 0xff);
    assert_eq!(bag(&mut wram), vec![BagItem { id: 4, count: 1 }]);

    // A count past BAG_ITEM_CAPACITY is not a bag.
    wram.bag(&[(4, 1)]);
    wram.set(ram::wNumBagItems, 21);
    assert!(bag(&mut wram).is_empty());
}

#[test]
fn an_npc_reads_its_map_tile_and_facing() {
    let mut wram = Wram::overworld();
    assert!(npcs(&mut wram).is_empty());

    // Oak's aide at (4, 3) facing left, and a second sprite at (1, 7) facing down.
    wram.npc(1, 0x0d, 4, 3, 0x08).npc(2, 0x02, 1, 7, 0x00);
    let sprites = npcs(&mut wram);
    assert_eq!(sprites.len(), 2);
    assert_eq!((sprites[0].slot, sprites[0].x, sprites[0].y), (1, 4, 3));
    assert_eq!(sprites[0].facing, Facing::Left);
    assert_eq!(sprites[0].picture, 0x0d);
    assert_eq!((sprites[1].x, sprites[1].y, sprites[1].facing), (1, 7, Facing::Down));

    // The coordinates are stored plus four, so a reader that forgot the bias would say (8, 7).
    assert_eq!(wram.peek(ram::wSpriteStateData2 + 16 + 4), 7);
    assert_eq!(wram.peek(ram::wSpriteStateData2 + 16 + 5), 8);

    // Slot 0 is the player and is never an NPC.
    wram.npc(0, 0x01, 3, 6, 0x00);
    assert_eq!(npcs(&mut wram).len(), 2);

    // A slot the map does not use carries $ff in its image index.
    wram.set(ram::wSpriteStateData1 + 16 + 2, 0xff);
    assert_eq!(npcs(&mut wram).len(), 1);

    // And wNumSprites bounds the walk.
    wram.set(ram::wNumSprites, 0);
    assert!(npcs(&mut wram).is_empty());
}

#[test]
fn a_still_sprite_is_an_object_and_everything_below_it_is_a_person() {
    let mut wram = Wram::overworld();
    // SPRITE_OAK ($03), SPRITE_SEEL ($3c, the last person), SPRITE_POKE_BALL ($3d, the first
    // still sprite) and SPRITE_GAMBLER_ASLEEP ($48, the last): FIRST_STILL_SPRITE is the line.
    wram.npc(1, 0x03, 1, 1, 0).npc(2, 0x3c, 2, 1, 0).npc(3, 0x3d, 3, 1, 0).npc(4, 0x48, 4, 1, 0);
    let people: Vec<u8> =
        npcs(&mut wram).iter().filter(|npc| npc.person()).map(|npc| npc.picture).collect();
    assert_eq!(people, vec![0x03, 0x3c]);
    let objects: Vec<u8> =
        npcs(&mut wram).iter().filter(|npc| !npc.person()).map(|npc| npc.picture).collect();
    assert_eq!(objects, vec![0x3d, 0x48]);
}

#[test]
fn the_sign_table_reads_y_then_x_with_no_bias() {
    let mut wram = Wram::overworld();
    assert!(signs(&mut wram).is_empty(), "a map with no bg_events has no signs");

    // `bg_event 2, 0, TEXT_A` emits `db \2, \1, \3`: Y first, and — unlike `object_event` —
    // with no +4, because the loader copies the two bytes straight across and
    // IsSpriteOrSignInFrontOfPlayer compares them against raw tile coordinates.
    wram.signs(&[(2, 0, 7), (5, 3, 8)]);
    assert_eq!(wram.peek(ram::wSignCoords), 0, "the Y comes first");
    assert_eq!(wram.peek(ram::wSignCoords + 1), 2, "and it is not biased by four");
    assert_eq!(
        signs(&mut wram),
        vec![Sign { x: 2, y: 0, text_id: 7 }, Sign { x: 5, y: 3, text_id: 8 }]
    );

    // MAX_BG_EVENTS bounds the walk, so a garbage count cannot read past the array.
    wram.set(ram::wNumSigns, 200);
    assert_eq!(signs(&mut wram).len(), 16);
}

#[test]
fn the_warp_table_reads_y_then_x() {
    let mut wram = Wram::overworld();
    assert!(warps(&mut wram).is_empty());

    // Red's bedroom declares `warp_event 7, 1, REDS_HOUSE_1F, 3`, which the macro emits as
    // Y then X: the ROM-gated test asserts the same two bytes on a real cartridge.
    wram.warps(&[(7, 1, 3, REDS_HOUSE_1F)]);
    assert_eq!(wram.peek(ram::wWarpEntries), 1, "the Y comes first");
    assert_eq!(wram.peek(ram::wWarpEntries + 1), 7);
    assert_eq!(
        warps(&mut wram),
        vec![Warp { x: 7, y: 1, destination_warp: 3, destination_map: REDS_HOUSE_1F }]
    );

    // MAX_WARP_EVENTS bounds the walk, so a garbage count cannot read past the array.
    wram.set(ram::wNumberOfWarps, 200);
    assert_eq!(warps(&mut wram).len(), 32);
}

#[test]
fn the_connection_bitmask_names_the_four_edges() {
    let mut wram = Wram::overworld();
    assert_eq!(connections(&mut wram), Connections::default());
    assert!(!connections(&mut wram).any());

    for (bits, expected) in [
        (poke::CONNECTION_EAST, Connections { east: true, ..Connections::default() }),
        (poke::CONNECTION_WEST, Connections { west: true, ..Connections::default() }),
        (poke::CONNECTION_SOUTH, Connections { south: true, ..Connections::default() }),
        (poke::CONNECTION_NORTH, Connections { north: true, ..Connections::default() }),
    ] {
        wram.set(ram::wCurMapConnections, bits);
        assert_eq!(connections(&mut wram), expected, "bits {bits:#04b}");
    }

    wram.set(ram::wCurMapConnections, 0x0f);
    assert_eq!(
        connections(&mut wram),
        Connections { north: true, south: true, east: true, west: true }
    );
}

#[test]
fn a_walkable_tile_is_one_in_the_tilesets_passable_list() {
    let mut wram = Wram::overworld();
    // The tile the player stands on, opened.
    wram.map_tile(3, 6, FLOOR);
    assert_eq!(walkable(&mut wram, 3, 6), Walkable::Yes);

    // Its neighbours are walls until they are opened.
    assert_eq!(walkable(&mut wram, 3, 5), Walkable::No);
    wram.map_tile(3, 5, FLOOR);
    assert_eq!(walkable(&mut wram, 3, 5), Walkable::Yes);

    // Every id in RedsHouse1_Coll is passable and one that is in no list is not.
    for tile in crate::pokemon_red::fake_wram::REDS_HOUSE_COLL {
        wram.map_tile(2, 6, tile);
        assert_eq!(walkable(&mut wram, 2, 6), Walkable::Yes, "tile {tile:#04x}");
    }
    wram.map_tile(2, 6, WALL_TILE);
    assert_eq!(walkable(&mut wram, 2, 6), Walkable::No);
}

#[test]
fn walkability_is_unknown_outside_the_screens_window() {
    let mut wram = Wram::new();
    // A map big enough to have tiles off screen: Pallet Town, 20 by 18 tiles, player at (5, 6).
    wram.started().map(0x00, 10, 9, 5, 6).facing(0).house_collision().fill_screen(FLOOR);

    // The window is x - 4 ..= x + 5 and y - 4 ..= y + 4.
    assert_eq!(walkable(&mut wram, 1, 6), Walkable::Yes);
    assert_eq!(walkable(&mut wram, 10, 6), Walkable::Yes);
    assert_eq!(walkable(&mut wram, 0, 6), Walkable::Unknown);
    assert_eq!(walkable(&mut wram, 11, 6), Walkable::Unknown);
    assert_eq!(walkable(&mut wram, 5, 2), Walkable::Yes);
    assert_eq!(walkable(&mut wram, 5, 10), Walkable::Yes);
    assert_eq!(walkable(&mut wram, 5, 1), Walkable::Unknown);
    assert_eq!(walkable(&mut wram, 5, 11), Walkable::Unknown);

    // The window follows the player, it is not the map: the same 8 by 8 indoor map is answerable
    // whole from its middle and only in part from a corner. An A* over it has to treat Unknown as
    // impassable and re-plan as the player moves, which is what the per-step check in
    // `docs/design/macros.md` section 4 already requires.
    let mut wram = Wram::overworld();
    wram.map(REDS_HOUSE_1F, 4, 4, 4, 4).fill_screen(FLOOR);
    for y in 0..8u8 {
        for x in 0..8u8 {
            assert_eq!(walkable(&mut wram, x, y), Walkable::Yes, "centred: ({x}, {y})");
        }
    }
    wram.map(REDS_HOUSE_1F, 4, 4, 3, 6);
    assert_eq!(walkable(&mut wram, 3, 2), Walkable::Yes, "four rows up is in the window");
    assert_eq!(walkable(&mut wram, 3, 1), Walkable::Unknown, "five is not");
    assert_eq!(walkable(&mut wram, 3, 0), Walkable::Unknown);
}

#[test]
fn walkability_is_no_off_the_map_and_unknown_when_the_screen_is_not_the_map() {
    let mut wram = Wram::overworld();
    wram.fill_screen(FLOOR);
    // Off the map is not a tile to stand on: the tiles a connection leads through are the map's
    // own edge rows, which are inside it.
    assert_eq!(walkable(&mut wram, 8, 6), Walkable::No);
    assert_eq!(walkable(&mut wram, 3, 8), Walkable::No);

    // While a text box is up the screen buffer holds the box.
    wram.set(ram::wFontLoaded, poke::BIT_FONT_LOADED);
    assert_eq!(walkable(&mut wram, 3, 6), Walkable::Unknown);

    // And in a battle it holds the battle.
    let mut wram = Wram::overworld();
    wram.fill_screen(FLOOR).set(ram::wIsInBattle, 1);
    assert_eq!(walkable(&mut wram, 3, 6), Walkable::Unknown);

    // An unloaded map answers nothing.
    let mut wram = Wram::overworld();
    wram.fill_screen(FLOOR).set(ram::wCurMapWidth, 0);
    assert_eq!(walkable(&mut wram, 3, 6), Walkable::Unknown);
}

#[test]
fn a_collision_pointer_outside_rom_bank_zero_is_not_followed() {
    let mut wram = Wram::overworld();
    wram.fill_screen(FLOOR);
    assert_eq!(walkable(&mut wram, 3, 6), Walkable::Yes);

    // Banked ROM: a MemoryReader reads whichever bank is mapped, which is not an answer.
    wram.set(ram::wTilesetCollisionPtr, 0x00).set(ram::wTilesetCollisionPtr + 1, 0x40);
    assert_eq!(walkable(&mut wram, 3, 6), Walkable::Unknown);

    // A null pointer, which is what an unloaded tileset header looks like.
    wram.set(ram::wTilesetCollisionPtr, 0).set(ram::wTilesetCollisionPtr + 1, 0);
    assert_eq!(walkable(&mut wram, 3, 6), Walkable::Unknown);

    // A list with no terminator inside the bound is unknown rather than an answer.
    let mut wram = Wram::overworld();
    wram.fill_screen(FLOOR);
    for offset in 0..80u16 {
        wram.set(0x1749 + offset, 0x42);
    }
    assert_eq!(walkable(&mut wram, 3, 6), Walkable::Unknown);
}

#[test]
fn the_live_implementation_answers_the_whole_trait() {
    // One pass over every method of `GameState`, through `PokeState`, so the trait's shape is
    // exercised the way agent B's executor will hold it.
    let mut wram = Wram::overworld();
    wram.fill_screen(FLOOR)
        .party_mon(0, 4, 7, 14, 22, 0, &[(10, 35)])
        .money(3000)
        .bag(&[(4, 1)])
        .npc(1, 0x0d, 4, 3, 0x08)
        .signs(&[(2, 0, 7)])
        .warps(&[(7, 1, 3, REDS_HOUSE_1F)])
        .set(ram::wCurMapConnections, poke::CONNECTION_SOUTH);

    let mut state = PokeState::new(&mut wram);
    assert_eq!(state.scene(), Scene::Overworld);
    assert_eq!(state.player().map(|player| (player.x, player.y)), Some((3, 6)));
    assert_eq!(state.map_size(), Some(MapSize { width: 8, height: 8 }));
    assert_eq!(state.party().mons.len(), 1);
    assert!(state.battle().is_none());
    assert_eq!(state.text_box(), TextBox { open: false, waiting: false });
    assert!(state.start_menu().is_none());
    assert!(state.shop().is_none());
    assert!(state.pc().is_none());
    assert_eq!(state.money(), 3000);
    assert_eq!(state.bag().len(), 1);
    assert_eq!(state.npcs().len(), 1);
    assert_eq!(state.signs().len(), 1);
    assert_eq!(state.walkable(3, 6), Walkable::Yes);
    assert_eq!(state.warps().len(), 1);
    assert!(state.connections().south);
}

/// A map ten blocks by nine — twenty tiles by eighteen, wider than the ten-by-nine window — with
/// a wall down one column of blocks, and a screen buffer that agrees with it.
///
/// The three tables the grid is decoded from, all synthetic: block ids in `wOverworldMap`, a
/// blockset in a ROM bank that is not bank 0, and a collision list in bank 0 where
/// `wTilesetCollisionPtr` points.
fn town() -> (Wram, Vec<u8>, Vec<[u8; 16]>) {
    Wram::town()
}

#[test]
fn the_whole_map_decodes_from_the_block_and_collision_tables() {
    let (mut wram, _, _) = town();
    let grid = map_grid(&mut wram).expect("a decodable map");
    assert_eq!((grid.width(), grid.height()), (20, 18));
    assert_eq!(grid.unknown_count(), 0);
    // The far corner of the map, which the window predicate cannot answer for at all: the grid
    // does, and that is the whole of section 15.
    assert_eq!(walkable(&mut wram, 19, 17), Walkable::Unknown);
    assert_eq!(grid.walkable(19, 17), Walkable::Yes);
    // The wall column, again outside the window.
    assert_eq!(grid.walkable(10, 17), Walkable::No);
    assert_eq!(grid.walkable(11, 17), Walkable::No);
    // Inside the window the two readings agree tile for tile, which is what the reader checks
    // itself with before it trusts a decode.
    for y in 0..18u8 {
        for x in 0..20u8 {
            if let Some(tile) = map_tile_id(&mut wram, x, y) {
                assert_eq!(grid.tile_id(x, y), Some(tile), "({x}, {y})");
                assert_eq!(grid.walkable(x, y), walkable(&mut wram, x, y), "({x}, {y})");
            }
        }
    }
}

#[test]
fn a_frame_mid_step_is_read_from_the_tile_the_screen_is_centred_on() {
    // Row 54 of `infra/docs/macros-traps.md`, measured on the cartridge: `wXCoord` and `wYCoord`
    // change at the *end* of a step, so for fifteen frames of every sixteen the screen buffer is
    // centred one tile ahead of them. The old cross-check compared the decode of the fly's tile
    // against the screen's reading of the tile ahead and refused; Pewter City decoded on 118 of
    // 120 standing frames and on none of the moving ones, and every walk the fly actually took was
    // planned over the ten-by-nine window instead.
    let (mut wram, blocks, blockset) = town();
    wram.mid_step(-1, 0, &blocks, &blockset);

    // The trap itself, stated as a reading: the two answers for a tile beside the fly disagree.
    let naive = map_tile_id(&mut wram, 2, 4);
    let grid = map_grid(&mut wram).expect("a mid-step frame still decodes");
    assert_ne!(grid.tile_id(2, 4), naive, "the screen is one column ahead of the coordinates");
    assert_eq!(grid.tile_id(1, 4), naive, "and that column is the one the step is landing on");

    // Which is what the reader now says out loud, for the stood ledger.
    assert_eq!(step_destination(&mut wram, &grid), Some((2, 4)));
    // Standing still there is no step to name.
    let (mut still, _, _) = town();
    let standing = map_grid(&mut still).expect("a decodable map");
    assert_eq!(step_destination(&mut still, &standing), None);
}

#[test]
fn a_decode_the_screen_disagrees_with_is_refused_mid_step_too() {
    // The check has to keep refusing a decode that is simply wrong, and the anchor search is what
    // could have weakened it: five anchors instead of one. A wrong stride, a wrong quadrant or a
    // half-loaded map agrees with none of them, because the whole neighbourhood has to agree under
    // one anchor rather than each tile finding an anchor of its own.
    let (mut wram, blocks, blockset) = town();
    wram.mid_step(-1, 0, &blocks, &blockset);
    wram.map_tile(3, 4, 0x77);
    assert_eq!(map_grid(&mut wram), Err(GridRefusal::ScreenDisagrees));
}

#[test]
fn a_decode_the_screen_disagrees_with_is_refused() {
    let (mut wram, _, _) = town();
    // One screen tile the block data does not account for: a wrong stride, a wrong quadrant or a
    // half-loaded map all look like this, and all of them answer plausibly.
    wram.map_tile(3, 4, 0x77);
    assert_eq!(map_grid(&mut wram), Err(GridRefusal::ScreenDisagrees));
}

#[test]
fn without_a_cartridge_behind_the_seam_there_is_no_grid() {
    let (mut wram, blocks, _) = town();
    // The same WRAM with no blockset in any bank: `read_rom` answers `None`, which is what every
    // reader that is not the emulator answers, and the grid narrows to nothing rather than
    // decoding the map out of whatever bytes were to hand.
    let mut bare = Wram::new();
    bare.started()
        .map(fake_wram::PALLET_TOWN, 10, 9, 3, 4)
        .facing(0)
        .house_collision()
        .map_blocks(&blocks)
        .fill_screen(WALL_TILE);
    assert_eq!(map_grid(&mut bare), Err(GridRefusal::NoBlockset));
    // And the window predicate still answers, which is the fallback the whole thing rests on.
    assert_eq!(walkable(&mut wram, 3, 4), Walkable::Yes);
}

#[test]
fn a_frame_that_is_not_showing_the_map_has_no_grid_to_check() {
    let (mut wram, _, _) = town();
    // `wOverworldMap` shares its bytes with the picture buffer (`ram/wram.asm`'s own union), so a
    // battle is exactly when the blocks under it belong to somebody else. Nothing can be
    // cross-checked then, and a decode nothing can check is refused.
    wram.battle(1);
    assert_eq!(map_grid(&mut wram), Err(GridRefusal::NoScreen));

    let (mut wram, _, _) = town();
    wram.dialogue_box();
    assert_eq!(map_grid(&mut wram), Err(GridRefusal::NoScreen));
}

#[test]
fn a_frame_with_no_map_header_has_no_grid() {
    let mut wram = Wram::new();
    wram.started();
    assert_eq!(map_grid(&mut wram), Err(GridRefusal::NoHeader));
    assert_eq!(GridRefusal::NoHeader.label(), "no map header");
}

#[test]
fn the_grid_is_decoded_once_per_map_and_dropped_on_arrival_somewhere_else() {
    let (mut wram, blocks, blockset) = town();
    let mut grids = MapGrids::default();
    {
        let mut state = PokeState::new(&mut wram).caching_grid(&mut grids);
        let first = state.map_grid().expect("a decodable map");
        let again = state.map_grid().expect("the cached map");
        assert!(std::sync::Arc::ptr_eq(&first, &again), "the second question is the same grid");
    }
    assert_eq!(grids.held(), Some(fake_wram::PALLET_TOWN));

    // Walking through a door: another map id, so the cache is somebody else's and is dropped.
    wram.map(fake_wram::OAKS_LAB, 10, 9, 3, 4)
        .map_blocks(&blocks)
        .screen_from_blocks(&blocks, &blockset);
    {
        let mut state = PokeState::new(&mut wram).caching_grid(&mut grids);
        let grid = state.map_grid().expect("a decodable map");
        assert_eq!(grid.map(), fake_wram::OAKS_LAB);
    }
    assert_eq!(grids.held(), Some(fake_wram::OAKS_LAB));
}

#[test]
fn a_state_with_no_cache_still_answers_and_a_state_with_no_cartridge_answers_none() {
    let (mut wram, _, _) = town();
    let mut state = PokeState::new(&mut wram);
    assert!(state.map_grid().is_some(), "no cache is slower, not blinder");

    let mut bare = Wram::overworld();
    let mut state = PokeState::new(&mut bare);
    assert!(state.map_grid().is_none(), "no blockset, no grid");
    // Which is the frame the window predicate is for.
    assert_eq!(state.walkable(3, 6), Walkable::No);
}

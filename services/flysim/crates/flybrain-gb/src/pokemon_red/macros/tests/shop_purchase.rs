use super::*;

#[test]
fn purchases_never_select_an_entry_from_the_sell_list() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::Selling);
    world.stock = vec![item::POTION, item::POKE_BALL, item::ANTIDOTE, item::REPEL];
    world.money = 9999;

    for kind in [
        MacroKind::BuyPotion,
        MacroKind::BuyBall,
        MacroKind::BuyAntidote,
        MacroKind::BuyRepel,
    ] {
        assert!(
            !precondition(kind, &mut world),
            "{kind:?} must not confirm a sale"
        );
    }
    assert_eq!(
        names(&Palette::for_scene(Scene::Shop, &mut world)),
        ["CONFIRM", "LEAVE"]
    );
    assert!(world.pulses.is_empty());
}

#[test]
fn a_purchase_dealt_before_entering_sell_is_refused_without_a_press() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::BuySellQuit);
    world.stock = vec![item::POKE_BALL];
    world.money = 3000;
    let (palette, slot) = pick(&mut world, MacroKind::BuyBall);
    world.list = List::Shop(ShopScreen::Selling);

    let mut machine = MacroMachine::new(1);
    assert!(machine.start(&palette, slot, &mut world).is_err());
    assert!(world.pulses.is_empty());
    assert!(machine.running().is_none());
}

/// Row 55: a purchase the counter's cursor cannot reach is not on the pad.
///
/// **What was live** (2026-09-22 19:02 UTC, the Pewter mart, ten brain minutes): `BUY ANTIDOTE
/// start` / `BUY ANTIDOTE blocked` every 0.8 s, 747 repeats of a sequence of one, nothing else
/// starting. Surveyed on the cartridge: the counter stocks seven items and ANTIDOTE is its
/// **fourth**, while the buy list's cursor walks rows `0, 1, 2` and then scrolls the window under
/// itself -- so the absolute index the script aimed at was above the list's own max and the step
/// returned `Blocked` on its first frame, with no button pressed and no byte changed. The offset
/// that would name the scrolled position is not a pinned address, so the fourth item and after
/// have no index this seam can aim at, and a macro that cannot run is not on the pad.
#[test]
fn a_purchase_past_the_cursors_reach_is_not_on_the_pad() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::Buying);
    world.cursor_max = 2;
    // Pewter's counter, in menu order, as `shop_stock` read it off the cartridge at the live
    // checkpoint: POKE BALL, POTION, ESCAPE ROPE, ANTIDOTE, BURN HEAL, AWAKENING, PARLYZ HEAL.
    // The four ids the palette has no constant for are the ones no button buys.
    world.stock = vec![item::POKE_BALL, item::POTION, 29, item::ANTIDOTE, 12, 14, 15];
    world.money = 9_999;
    assert_eq!(
        names(&Palette::for_scene(Scene::Shop, &mut world)),
        ["CONFIRM", "BUY POTION", "BUY BALL", "LEAVE"],
        "the Antidote is Pewter's fourth item and the cursor cannot reach it"
    );
    // The live wallet, where the Antidote was the only thing money allowed: the pad is the two
    // presses that get out of the counter, and it is not empty.
    world.money = 104;
    assert_eq!(
        names(&Palette::for_scene(Scene::Shop, &mut world)),
        ["CONFIRM", "LEAVE"],
        "nothing in reach is affordable, so nothing is dealt but the ways out"
    );
    assert!(world.pulses.is_empty());
}

/// Row 55, the other half: the clerk talking is not a list, so no purchase starts on it.
///
/// `wListMenuID` keeps `PRICEDITEMLISTMENU` across the mart's own text
/// (`docs/design/macros-wram.md` section 7), and the cursor bytes left behind belong to a
/// two-option box. A purchase dealt on such a frame navigates a menu that is not on screen, which
/// is section 12.11's rule in the one scene it had not reached -- so the seam names the screen and
/// the palette deals the presses that *do* advance it.
#[test]
fn the_clerks_own_text_box_deals_no_purchase_and_reports_no_list() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::Talking);
    world.cursor_max = 1;
    world.stock = vec![item::POKE_BALL, item::POTION, item::ANTIDOTE];
    world.money = 9_999;

    for kind in [
        MacroKind::BuyPotion,
        MacroKind::BuyBall,
        MacroKind::BuyAntidote,
        MacroKind::BuyRepel,
    ] {
        assert!(!precondition(kind, &mut world), "{kind:?} has no list to navigate");
    }
    assert_eq!(names(&Palette::for_scene(Scene::Shop, &mut world)), ["CONFIRM", "LEAVE"]);
    assert!(listing(&mut world).is_none(), "nothing is accepting list input");
    assert!(world.pulses.is_empty());
}

/// And a purchase that was dealt before the counter changed under it presses nothing.
///
/// The same shape as `a_purchase_dealt_before_entering_sell_is_refused_without_a_press`, for the
/// reach: the script asks [`stock_index`] again and refuses, rather than aiming a cursor step at
/// an index the list cannot hold and reporting `blocked` a frame later.
#[test]
fn a_purchase_out_of_reach_by_the_time_it_starts_is_refused_without_a_press() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::Buying);
    world.cursor_max = 2;
    world.stock = vec![item::ANTIDOTE, item::POKE_BALL];
    world.money = 9_999;
    let (palette, slot) = pick(&mut world, MacroKind::BuyAntidote);
    // The counter the fly is standing at turns out to be Pewter's, where the Antidote is fourth.
    world.stock = vec![item::POKE_BALL, item::POTION, 29, item::ANTIDOTE];

    let mut machine = MacroMachine::new(1);
    assert!(machine.start(&palette, slot, &mut world).is_err());
    assert!(world.pulses.is_empty());
    assert!(machine.running().is_none());
}

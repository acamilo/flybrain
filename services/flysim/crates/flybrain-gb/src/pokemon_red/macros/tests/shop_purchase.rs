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

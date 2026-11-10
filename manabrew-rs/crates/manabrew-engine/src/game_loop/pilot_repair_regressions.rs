//! Archived rules use the exact retained archive scripts, with small public-state fixtures.
use super::*;
use crate::agent::PassAgent;
use forge_carddb::parse_card_script;

fn add(g: &mut GameState, text: &str, player: PlayerId, zone: ZoneType) -> CardId {
    let rules = parse_card_script(text).unwrap();
    let id = g.create_card(crate::card::Card::from_rules(&rules, player));
    g.move_card(id, zone, player);
    id
}
fn agents() -> Vec<Box<dyn PlayerAgent>> {
    (0..4)
        .map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>)
        .collect()
}
fn settle(gl: &mut GameLoop, g: &mut GameState, agents: &mut [Box<dyn PlayerAgent>]) {
    for _ in 0..40 {
        gl.process_triggers(g, agents);
        if g.stack.is_empty() {
            return;
        }
        gl.resolve_stack(g, agents);
    }
    panic!("fixture did not settle");
}
fn effect(
    gl: &mut GameLoop,
    g: &mut GameState,
    agents: &mut [Box<dyn PlayerAgent>],
    host: CardId,
    raw: &str,
) {
    let sa = crate::spellability::build_spell_ability(g, host, raw, g.card(host).controller);
    gl.resolve_single_effect(g, agents, &sa, None);
    settle(gl, g, agents);
}
const GIADA: &str = r###"Name:Giada, Font of Hope
ManaCost:1 W
Types:Legendary Creature Angel
PT:2/2
K:Flying
K:Vigilance
K:ETBReplacement:Other:AddExtraCounter:Mandatory:Battlefield:Creature.Angel+YouCtrl+Other
SVar:AddExtraCounter:DB$ PutCounter | ETB$ True | Defined$ ReplacedCard | CounterType$ P1P1 | CounterNum$ X | SpellDescription$ Each other Angel you control enters with an additional +1/+1 counter on it for each Angel you already control.
SVar:X:Count$Valid Angel.YouCtrl
A:AB$ Mana | Cost$ T | Produced$ W | RestrictValid$ Spell.Angel | SpellDescription$ Add {W}. Spend this mana only to cast an Angel spell.
DeckHas:Ability$Counters
DeckNeeds:Type$Angel
Oracle:Flying, vigilance\nEach other Angel you control enters with an additional +1/+1 counter on it for each Angel you already control.\n{T}: Add {W}. Spend this mana only to cast an Angel spell."###;
const PARAGON: &str = r###"Name:Serra Paragon
ManaCost:2 W W
Types:Creature Angel
PT:3/4
K:Flying
S:Mode$ Continuous | Affected$ Permanent.YouOwn+cmcLE3,Land.YouOwn | ValidAfterStack$ Spell.cmcLE3 | Condition$ PlayerTurn | MayPlay$ True | MayPlayLimit$ 1 | EffectZone$ Battlefield | AffectedZone$ Graveyard | Description$ Once during each of your turns, you may play a land from your graveyard or cast a permanent spell with mana value 3 or less from your graveyard. If you do, it gains "When this permanent is put into a graveyard from the battlefield, exile it and you gain 2 life."
T:Mode$ SpellCast | ValidCard$ Card | ValidSA$ Spell.MayPlaySource | Static$ True | Execute$ DBAnimate | TriggerZones$ Battlefield | Secondary$ True | TriggerDescription$ Once during each of your turns, you may play a land from your graveyard or cast a permanent spell with mana value 3 or less from your graveyard. If you do, it gains "When this permanent is put into a graveyard from the battlefield, exile it and you gain 2 life."
T:Mode$ LandPlayed | ValidCard$ Land | ValidSA$ SpellAbility.MayPlaySource | Static$ True | Execute$ DBAnimate | TriggerZones$ Battlefield | Secondary$ True | TriggerDescription$ Once during each of your turns, you may play a land from your graveyard or cast a permanent spell with mana value 3 or less from your graveyard. If you do, it gains "When this permanent is put into a graveyard from the battlefield, exile it and you gain 2 life."
SVar:DBAnimate:DB$ Animate | Defined$ TriggeredCardLKICopy | Duration$ Permanent | Triggers$ TrigDieExile
SVar:TrigDieExile:Mode$ ChangesZone | Origin$ Battlefield | Destination$ Graveyard | ValidCard$ Card.Self | Execute$ TrigExile | TriggerDescription$ When this permanent is put into a graveyard from the battlefield, exile it and you gain 2 life.
SVar:TrigExile:DB$ ChangeZone | Defined$ TriggeredNewCardLKICopy | Origin$ Graveyard | Destination$ Exile | SubAbility$ DBGainLife
SVar:DBGainLife:DB$ GainLife | LifeAmount$ 2
DeckHas:Ability$Graveyard|LifeGain
Oracle:Flying\nOnce during each of your turns, you may play a land from your graveyard or cast a permanent spell with mana value 3 or less from your graveyard. If you do, it gains "When this permanent is put into a graveyard from the battlefield, exile it and you gain 2 life.""###;
const SUNBORN: &str = r###"Name:Exalted Sunborn
ManaCost:3 W W
Types:Creature Angel Wizard
PT:4/5
K:Flying
K:Lifelink
R:Event$ CreateToken | ActiveZones$ Battlefield | ValidToken$ Card.YouCtrl | ReplaceWith$ DoubleToken | Description$ If one or more tokens would be created under your control, twice that many of those tokens are created instead.
SVar:DoubleToken:DB$ ReplaceToken | Type$ Amount
K:Warp:1 W
Oracle:Flying, lifelink\nIf one or more tokens would be created under your control, twice that many of those tokens are created instead.\nWarp {1}{W} (You may cast this card from your hand for its warp cost. Exile this creature at the beginning of the next end step, then you may cast it from exile on a later turn.)"###;
const BALLISTA: &str = r###"Name:Walking Ballista
ManaCost:X X
Types:Artifact Creature Construct
PT:0/0
K:etbCounter:P1P1:X
A:AB$ PutCounter | Cost$ 4 | CounterType$ P1P1 | CounterNum$ 1 | SpellDescription$ Put a +1/+1 counter on CARDNAME.
A:AB$ DealDamage | AILogic$ Triskelion | Cost$ SubCounter<1/P1P1> | ValidTgts$ Any | NumDmg$ 1 | SpellDescription$ It deals 1 damage to any target.
SVar:X:Count$xPaid
DeckHas:Ability$Counters
DeckHints:Ability$Proliferate
Oracle:Walking Ballista enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on Walking Ballista.\nRemove a +1/+1 counter from Walking Ballista: It deals 1 damage to any target."###;
const VALKYRIE: &str = r###"Name:Righteous Valkyrie
ManaCost:2 W
Types:Creature Angel Cleric
PT:2/4
K:Flying
T:Mode$ ChangesZone | Origin$ Any | Destination$ Battlefield | ValidCard$ Cleric.Other+YouCtrl,Angel.Other+YouCtrl | TriggerZones$ Battlefield | Execute$ TrigGainLife | TriggerDescription$ Whenever another Angel or Cleric you control enters, you gain life equal to that creature's toughness.
SVar:TrigGainLife:DB$ GainLife | Defined$ You | LifeAmount$ X
SVar:X:TriggeredCard$CardToughness
S:Mode$ Continuous | CheckSVar$ Z | SVarCompare$ GEY | Affected$ Creature.YouCtrl | AddPower$ 2 | AddToughness$ 2 | Description$ As long as you have at least 7 life more than your starting life total, creatures you control get +2/+2.
SVar:Z:Count$YourLifeTotal
SVar:Y:Count$YourStartingLife/Plus.7
DeckHas:Ability$LifeGain
DeckHints:Type$Angel|Cleric
Oracle:Flying\nWhenever another Angel or Cleric you control enters, you gain life equal to that creature's toughness.\nAs long as you have at least 7 life more than your starting life total, creatures you control get +2/+2."###;

#[test]
fn simultaneous_entry_counts_prebatch_angels_and_separate_resolutions_advance() {
    std::thread::Builder::new().stack_size(32*1024*1024).spawn(|| {
    for (giada_present, multiplied, copy, expected) in [(true,true,false,10),(true,false,false,9),(false,true,false,0),(true,true,true,10)] {
        let p = PlayerId(0);
        let mut g = GameState::new(&["A","B","C","D"],40);
        let giada = add(&mut g, GIADA, p, ZoneType::Battlefield);
        if !giada_present { g.move_card(giada, ZoneType::Graveyard, p); }
        if multiplied { add(&mut g, SUNBORN, p, ZoneType::Battlefield); }
        for _ in 0..8 { add(&mut g,"Name:Peer\nTypes:Creature Angel\nPT:1/1",p,ZoneType::Battlefield); }
        let host = add(&mut g,"Name:Maker\nTypes:Artifact",p,ZoneType::Battlefield);
        let mut gl = GameLoop::new(4); let mut agents = agents();
        gl.trigger_handler.reset_active_triggers(&g);
        if copy {
            let original = g.cards.iter().find(|c|c.card_name == "Peer").unwrap().id;
            let mut sa = crate::spellability::build_spell_ability(&g, host,"DB$ CopyPermanent | NumCopies$ 1",p);
            sa.target_chosen.target_card=Some(original);
            gl.resolve_single_effect(&mut g,&mut agents,&sa,None);
        } else {
            effect(&mut gl,&mut g,&mut agents,host,"DB$ Token | TokenAmount$ 1 | TokenName$ Angel | TokenTypes$ Creature Angel | TokenPower$ 4 | TokenToughness$ 4 | TokenKeywords$ Flying");
        }
        let tokens:Vec<_>=g.cards.iter().filter(|c|c.is_token && c.zone==ZoneType::Battlefield).map(|c|c.id).collect();
        assert_eq!(tokens.len(), if multiplied {2} else {1});
        for id in &tokens { assert_eq!(g.card(*id).counter_count(&crate::card::CounterType::P1P1), expected); }
        effect(&mut gl,&mut g,&mut agents,host,"DB$ Token | TokenAmount$ 1 | TokenName$ Angel | TokenTypes$ Creature Angel | TokenPower$ 4 | TokenToughness$ 4");
        let later:Vec<_>=g.cards.iter().filter(|c|c.is_token && !tokens.contains(&c.id)).collect();
        for token in later {assert_eq!(token.counter_count(&crate::card::CounterType::P1P1), if giada_present {expected+tokens.len() as i32} else {0});}
    }
    }).unwrap().join().unwrap();
}

#[test]
fn multiplied_entry_lifegain_uses_equal_completed_toughness() {
    std::thread::Builder::new().stack_size(32*1024*1024).spawn(|| {
        let p=PlayerId(0);let mut g=GameState::new(&["A","B","C","D"],40);
        add(&mut g,GIADA,p,ZoneType::Battlefield);
        add(&mut g,SUNBORN,p,ZoneType::Battlefield);
        let watcher=add(&mut g,VALKYRIE,p,ZoneType::Battlefield);
        // Keep the Valkyrie anthem inactive before and after this batch.
        g.player_mut(p).life=1;
        let mut gl=GameLoop::new(4);let mut agents=agents();gl.trigger_handler.reset_active_triggers(&g);
        effect(&mut gl,&mut g,&mut agents,watcher,"DB$ Token | TokenAmount$ 1 | TokenName$ Angel | TokenTypes$ Creature Angel | TokenPower$ 4 | TokenToughness$ 4");
        let tokens:Vec<_>=g.cards.iter().filter(|c|c.is_token).collect();assert_eq!(tokens.len(),2);
        assert_eq!(tokens[0].counter_count(&crate::card::CounterType::P1P1),3);
        assert_eq!(tokens[1].counter_count(&crate::card::CounterType::P1P1),3);
        assert_eq!(g.player(p).life,15,"two 7 toughness entries, each triggering once");
    }).unwrap().join().unwrap();
}

#[test]
fn recovery_permissions_are_origin_turn_usage_and_announced_x_specific() {
    let p = PlayerId(0);
    let mut g = GameState::new(&["A", "B", "C", "D"], 40);
    let source = add(&mut g, PARAGON, p, ZoneType::Battlefield);
    let ballista = add(&mut g, BALLISTA, p, ZoneType::Graveyard);
    g.turn.phase = PhaseType::Main1;
    let mut gl = GameLoop::new(4);
    gl.pool_mut(p)
        .add(forge_foundation::mana::ManaAtom::COLORLESS, 6);
    assert!(gl
        .get_playable_cards(&g, p, false)
        .iter()
        .any(|o| o.card_id == ballista));
    assert_eq!(crate::mana::maximum_cast_x(&g, gl.pool(p), ballista, p), 1);
    let mut sa = crate::spellability::build_spell_ability_for_card_cast(&g, ballista, p);
    for (x, expected) in [(0, true), (1, true), (2, false), (3, false)] {
        sa.x_mana_cost_paid = x;
        assert_eq!(
            crate::staticability::static_ability_continuous::play_permission(
                &g,
                p,
                g.card(ballista),
                &sa
            )
            .is_some(),
            expected,
            "X={x}"
        );
    }
    g.turn.active_player = PlayerId(1);
    assert!(!gl
        .get_playable_cards(&g, p, false)
        .iter()
        .any(|o| o.card_id == ballista));
    g.turn.active_player = p;
    crate::staticability::static_ability_continuous::consume_play_permission(&mut g, source, 0);
    assert!(!gl
        .get_playable_cards(&g, p, false)
        .iter()
        .any(|o| o.card_id == ballista));
    g.turn.turn_number += 1;
    assert!(gl
        .get_playable_cards(&g, p, false)
        .iter()
        .any(|o| o.card_id == ballista));
    crate::staticability::static_ability_continuous::consume_play_permission(&mut g, source, 0);
    g.move_card(source, ZoneType::Exile, p);
    g.move_card(source, ZoneType::Battlefield, p);
    assert!(
        gl.get_playable_cards(&g, p, false)
            .iter()
            .any(|o| o.card_id == ballista),
        "new source object resets usage"
    );
}

#[test]
fn only_permission_land_gets_object_death_trigger_and_source_death_does_not_revoke_it() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            for (origin, owner, grant) in [
                (ZoneType::Hand, 0, false),
                (ZoneType::Hand, 1, false),
                (ZoneType::Graveyard, 0, true),
            ] {
                let p = PlayerId(0);
                let owner = PlayerId(owner);
                let mut g = GameState::new(&["A", "B", "C", "D"], 40);
                let source = add(&mut g, PARAGON, p, ZoneType::Battlefield);
                let land = add(
                    &mut g,
                    "Name:Plains\nTypes:Basic Land Plains\nSVar:DBGainLife:DB$ GainLife | LifeAmount$ 99",
                    owner,
                    origin,
                );
                let mut gl = GameLoop::new(4);
                let mut agents = agents();
                gl.trigger_handler.reset_active_triggers(&g);
                assert!(gl
                    .play_land(
                        &mut g,
                        &mut agents,
                        owner,
                        land,
                        "Plains",
                        crate::agent::PlayCardMode::Normal
                    )
                    .is_some());
                settle(&mut gl, &mut g, &mut agents);
                assert_eq!(g.card(land).object_trigger_count, usize::from(grant));
                assert_eq!(
                    g.card(source).object_trigger_count,
                    0,
                    "grant must not attach to permission source"
                );
                gl.step_cleanup(&mut g, &mut agents);
                assert_eq!(
                    g.card(land).object_trigger_count,
                    usize::from(grant),
                    "permanent grant survives cleanup"
                );
                // Remove source first, then move the granted permanent through production ChangeZone.
                effect(
                    &mut gl,
                    &mut g,
                    &mut agents,
                    source,
                    "DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Graveyard",
                );
                effect(
                    &mut gl,
                    &mut g,
                    &mut agents,
                    land,
                    "DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Graveyard",
                );
                assert_eq!(
                    g.card(land).zone,
                    if grant {
                        ZoneType::Exile
                    } else {
                        ZoneType::Graveyard
                    }
                );
                assert_eq!(g.player(owner).life, if grant { 42 } else { 40 });
                assert_eq!(g.card(land).object_trigger_count, 0);
                assert_eq!(
                    g.card(source).zone,
                    ZoneType::Graveyard,
                    "source must not exile itself"
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn paid_recovery_caps_x_then_grants_to_spell_and_exiles_only_that_object_on_death() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            for zone in [ZoneType::Hand, ZoneType::Graveyard] {
                let p = PlayerId(0);
                let mut g = GameState::new(&["A", "B", "C", "D"], 40);
                g.turn.phase = PhaseType::Main1;
                let source = add(&mut g, PARAGON, p, ZoneType::Battlefield);
                let ballista = add(&mut g, BALLISTA, p, zone);
                let mut gl = GameLoop::new(4);
                gl.pool_mut(p)
                    .add(forge_foundation::mana::ManaAtom::COLORLESS, 6);
                let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4)
                    .map(|_| {
                        Box::new(super::actual_mana_spend_regression::AutoAgent(true, false))
                            as Box<dyn PlayerAgent>
                    })
                    .collect();
                gl.trigger_handler.reset_active_triggers(&g);
                let sa = crate::spellability::build_spell_ability_for_card_cast(&g, ballista, p);
                assert!(gl
                    .cast_card_spell_ability(&mut g, &mut agents, p, sa, false)
                    .is_some());
                let expected = if zone == ZoneType::Graveyard { 1 } else { 3 };
                let entry = g
                    .stack
                    .iter()
                    .find(|e| e.spell_ability.source == Some(ballista))
                    .unwrap();
                assert_eq!(entry.spell_ability.x_mana_cost_paid, expected);
                assert_eq!(g.player(p).mana_expended_this_turn, expected as i32 * 2);
                settle(&mut gl, &mut g, &mut agents);
                assert_eq!(
                    g.card(ballista)
                        .counter_count(&crate::card::CounterType::P1P1),
                    expected as i32
                );
                assert_eq!(
                    g.card(ballista).object_trigger_count,
                    usize::from(zone == ZoneType::Graveyard)
                );
                assert_eq!(g.card(source).object_trigger_count, 0);
                effect(
                    &mut gl,
                    &mut g,
                    &mut agents,
                    source,
                    "DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Exile",
                );
                effect(
                    &mut gl,
                    &mut g,
                    &mut agents,
                    ballista,
                    "DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Graveyard",
                );
                assert_eq!(
                    g.card(ballista).zone,
                    if zone == ZoneType::Graveyard {
                        ZoneType::Exile
                    } else {
                        ZoneType::Graveyard
                    }
                );
                assert_eq!(
                    g.player(p).life,
                    if zone == ZoneType::Graveyard { 42 } else { 40 }
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn feasible_x_uses_reductions_restricted_mana_and_floating_payment() {
    let p = PlayerId(0);
    for (restricted, reduced, expected) in [(false, false, 2), (true, false, 1), (false, true, 3)] {
        let mut g = GameState::new(&["A", "B", "C", "D"], 40);
        let id = add(&mut g, BALLISTA, p, ZoneType::Hand);
        let script = if restricted {
            "Name:Restricted\nTypes:Creature Angel\nPT:2/2\nA:AB$ Mana | Cost$ T | Produced$ W | Amount$ 2 | RestrictValid$ Spell.Angel"
        } else {
            "Name:Rock\nTypes:Artifact\nA:AB$ Mana | Cost$ T | Produced$ C | Amount$ 2"
        };
        let mana = add(&mut g, script, p, ZoneType::Battlefield);
        g.card_mut(mana).set_summoning_sick(false);
        if reduced {
            add(&mut g,"Name:Reducer\nTypes:Artifact\nS:Mode$ ReduceCost | ValidCard$ Artifact | Type$ Spell | Amount$ 2",p,ZoneType::Battlefield);
        }
        let mut pool = crate::mana::ManaPool::new();
        pool.add(forge_foundation::mana::ManaAtom::COLORLESS, 2);
        assert_eq!(crate::mana::maximum_cast_x(&g, &pool, id, p), expected);
    }
}

#[test]
fn granted_death_respects_blink_replacement_controller_and_trigger_doubling() {
    std::thread::Builder::new().stack_size(32*1024*1024).spawn(|| {
    for mode in ["blink","exile-replacement","stolen","double","destroy-all","copy"] {
        let p=PlayerId(0);let mut g=GameState::new(&["A","B","C","D"],40);
        let source=add(&mut g,PARAGON,p,ZoneType::Battlefield);
        let land=add(&mut g,"Name:Plains\nTypes:Basic Land Plains",p,ZoneType::Graveyard);
        let mut gl=GameLoop::new(4);let mut agents=agents();gl.trigger_handler.reset_active_triggers(&g);
        assert!(gl.play_land(&mut g,&mut agents,p,land,"Plains",crate::agent::PlayCardMode::Normal).is_some());
        settle(&mut gl,&mut g,&mut agents);assert_eq!(g.card(land).object_trigger_count,1);
        let other=add(&mut g,BALLISTA,p,ZoneType::Graveyard);
        g.turn.phase=PhaseType::Main1;
        assert!(!gl.get_playable_cards(&g,p,false).iter().any(|o|o.card_id==other),"land and spell share permission usage");
        if mode=="copy" {
            let mut sa=crate::spellability::build_spell_ability(&g,source,"DB$ CopyPermanent | NumCopies$ 1",p);
            sa.target_chosen.target_card=Some(land);gl.resolve_single_effect(&mut g,&mut agents,&sa,None);
            settle(&mut gl,&mut g,&mut agents);
            let copied=g.cards.iter().find(|card|card.is_token).unwrap().id;
            effect(&mut gl,&mut g,&mut agents,copied,"DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Graveyard");
            assert_eq!(g.player(p).life,40,"object grant is not a copiable characteristic");
        }
        if mode=="blink" {
            effect(&mut gl,&mut g,&mut agents,land,"DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Exile");
            g.move_card(land,ZoneType::Battlefield,p);assert_eq!(g.card(land).object_trigger_count,0);
        } else if mode=="exile-replacement" {
            add(&mut g,"Name:Redirect\nTypes:Enchantment\nR:Event$ Moved | ActiveZones$ Battlefield | Origin$ Battlefield | Destination$ Graveyard | ValidCard$ Card | NewDestination$ Exile",p,ZoneType::Battlefield);
        } else if mode=="stolen" { g.change_controller(land,PlayerId(1)); }
        else if mode=="double" {
            add(&mut g,"Name:Doubler\nTypes:Artifact\nS:Mode$ Panharmonicon | ValidCard$ Permanent.YouCtrl | ValidMode$ ChangesZone | Origin$ Battlefield | Destination$ Graveyard",p,ZoneType::Battlefield);
        }
        if mode=="destroy-all" {
            effect(&mut gl,&mut g,&mut agents,source,"DB$ DestroyAll | ValidCards$ Land");
        } else {
            effect(&mut gl,&mut g,&mut agents,land,"DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Graveyard");
        }
        assert_eq!(g.card(land).zone,if mode=="blink"{ZoneType::Graveyard}else{ZoneType::Exile},"{mode}");
        assert_eq!(g.player(p).life,match mode {"stolen"|"blink"|"exile-replacement"=>40,"double"=>44,_=>42},"{mode}");
        assert_eq!(g.player(PlayerId(1)).life,if mode=="stolen"{42}else{40},"{mode}");
        assert_eq!(g.card(land).object_trigger_count,0);
    }
    }).unwrap().join().unwrap();
}

#[test]
fn recovery_grant_follows_counter_payment_death_and_does_not_regrant_on_source_self_death() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let p = PlayerId(0);
            let mut g = GameState::new(&["A", "B", "C", "D"], 40);
            g.turn.phase = PhaseType::Main1;
            let source = add(&mut g, PARAGON, p, ZoneType::Battlefield);
            let ballista = add(&mut g, BALLISTA, p, ZoneType::Graveyard);
            let mut gl = GameLoop::new(4);
            gl.pool_mut(p)
                .add(forge_foundation::mana::ManaAtom::COLORLESS, 2);
            let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4)
                .map(|_| {
                    Box::new(super::actual_mana_spend_regression::AutoAgent(true, false))
                        as Box<dyn PlayerAgent>
                })
                .collect();
            gl.trigger_handler.reset_active_triggers(&g);
            let sa = crate::spellability::build_spell_ability_for_card_cast(&g, ballista, p);
            assert!(gl
                .cast_card_spell_ability(&mut g, &mut agents, p, sa, false)
                .is_some());
            settle(&mut gl, &mut g, &mut agents);
            let ability = g
                .card(ballista)
                .activated_abilities
                .iter()
                .find(|a| a.ability_text.contains("DealDamage"))
                .unwrap()
                .clone();
            assert!(gl.play_activated_ability_on_stack(&mut g, &mut agents, p, ballista, &ability));
            // The final counter was paid as a cost; SBAs happen before damage resolves.
            super::super::check_sba(&mut g, &mut gl.trigger_handler, &mut agents);
            settle(&mut gl, &mut g, &mut agents);
            assert_eq!(g.card(ballista).zone, ZoneType::Exile);
            assert_eq!(
                g.player(p).life,
                41,
                "gain two, and actual pinger target deals one to player0"
            );
            assert!(!gl
                .get_playable_cards(&g, p, false)
                .iter()
                .any(|o| o.card_id == ballista));
            effect(
                &mut gl,
                &mut g,
                &mut agents,
                source,
                "DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Graveyard",
            );
            assert_eq!(g.card(source).zone, ZoneType::Graveyard);
            assert_eq!(g.player(p).life, 41);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn entry_counts_are_order_independent_with_counter_and_token_replacements() {
    std::thread::Builder::new().stack_size(32*1024*1024).spawn(|| {
    for scripts in ["a,b","b,a"] {for counter_double in [false,true] {
        let p=PlayerId(0);let mut g=GameState::new(&["A","B","C","D"],40);
        add(&mut g,GIADA,p,ZoneType::Battlefield);let host=add(&mut g,SUNBORN,p,ZoneType::Battlefield);
        if counter_double {add(&mut g,"Name:Counter replacement\nTypes:Enchantment\nR:Event$ AddCounter | ActiveZones$ Battlefield | ValidCard$ Card.YouCtrl | ValidCounterType$ P1P1 | ReplaceWith$ DoubleCounters",p,ZoneType::Battlefield);}
        let mut gl=GameLoop::new(4);let mut agents=agents();
        for (key,body) in [("a","Name:A\nTypes:Creature Angel\nPT:4/4"),("b","Name:B\nTypes:Creature Angel\nPT:3/3")] {
            gl.register_token(key,crate::card::Card::from_rules(&parse_card_script(body).unwrap(),p));
        }
        gl.trigger_handler.reset_active_triggers(&g);
        effect(&mut gl,&mut g,&mut agents,host,&format!("DB$ Token | TokenAmount$ 1 | TokenScript$ {scripts}"));
        let tokens:Vec<_>=g.cards.iter().filter(|c|c.is_token && c.zone==ZoneType::Battlefield).collect();
        assert_eq!(tokens.len(),4);
        for token in tokens {assert_eq!(token.counter_count(&crate::card::CounterType::P1P1),if counter_double{4}else{2},"{scripts}, double={counter_double}");}
    }}
    }).unwrap().join().unwrap();
}

const ELSPETH: &str = r###"Name:Elspeth, Storm Slayer
ManaCost:3 W W
Types:Legendary Planeswalker Elspeth
Loyalty:5
R:Event$ CreateToken | ActiveZones$ Battlefield | ValidToken$ Card.YouCtrl | ReplaceWith$ DoubleToken | Description$ If one or more tokens would be created under your control, twice that many of those tokens are created instead.
SVar:DoubleToken:DB$ ReplaceToken | Type$ Amount
A:AB$ Token | Cost$ AddCounter<1/LOYALTY> | Planeswalker$ True | TokenScript$ w_1_1_soldier | SpellDescription$ Create a 1/1 white Soldier creature token.
A:AB$ PutCounterAll | Cost$ AddCounter<0/LOYALTY> | ValidCards$ Creature.YouCtrl | CounterType$ P1P1 | CounterNum$ 1 | Planeswalker$ True | SubAbility$ DBPumpAll | SpellDescription$ Put a +1/+1 counter on each creature you control. Those creatures gain flying until your next turn.
SVar:DBPumpAll:DB$ PumpAll | ValidCards$ Creature.YouCtrl | KW$ Flying | Duration$ UntilYourNextTurn
A:AB$ Destroy | Cost$ SubCounter<3/LOYALTY> | Planeswalker$ True | Ultimate$ True | ValidTgts$ Creature.OppCtrl+cmcGE3 | TgtPrompt$ Select target creature an opponent controls with mana value 3 or greater | SpellDescription$ Destroy target creature an opponent controls with mana value 3 or greater.
Oracle:If one or more tokens would be created under your control, twice that many of those tokens are created instead.\n[+1]: Create a 1/1 white Soldier creature token.\n[0]: Put a +1/+1 counter on each creature you control. Those creatures gain flying until your next turn.\n[-3]: Destroy target creature an opponent controls with mana value 3 or greater."###;

#[test]
fn archived_conversion_changes_ready_army_and_keeps_flying_through_opponents_turn() {
    std::thread::Builder::new().stack_size(32*1024*1024).spawn(|| {
    for mode in [0,1] {
        let p=PlayerId(0);let mut g=GameState::new(&["A","B","C","D"],40);g.turn.phase=PhaseType::Main1;
        let walker=add(&mut g,ELSPETH,p,ZoneType::Battlefield);
        let army:Vec<_>=(0..12).map(|_|{let id=add(&mut g,"Name:Cat\nTypes:Creature Cat\nPT:1/1",p,ZoneType::Battlefield);g.card_mut(id).set_summoning_sick(false);id}).collect();
        let mut gl=GameLoop::new(4);let mut agents:Vec<Box<dyn PlayerAgent>>=(0..4).map(|_|Box::new(super::actual_mana_spend_regression::AutoAgent(true,false)) as Box<dyn PlayerAgent>).collect();
        gl.register_token("w_1_1_soldier",crate::card::Card::from_rules(&parse_card_script("Name:Soldier\nTypes:Creature Soldier\nPT:1/1").unwrap(),p));
        gl.trigger_handler.reset_active_triggers(&g);
        let ability=g.card(walker).activated_abilities.iter().find(|a|a.ability_index==mode).unwrap().clone();
        assert!(gl.play_activated_ability_on_stack(&mut g,&mut agents,p,walker,&ability));settle(&mut gl,&mut g,&mut agents);
        assert_eq!(g.card(walker).counter_count(&crate::card::CounterType::Loyalty),if mode==0{6}else{5});
        for &id in &army {assert_eq!(g.card(id).power(),if mode==1{2}else{1});assert_eq!(g.card(id).has_keyword("Flying"),mode==1);assert!(!g.card(id).summoning_sick);}
        assert_eq!(g.cards.iter().filter(|c|c.is_token).count(),if mode==0{2}else{0});
        gl.step_cleanup(&mut g,&mut agents);g.turn.active_player=PlayerId(1);
        for &id in &army {assert_eq!(g.card(id).has_keyword("Flying"),mode==1,"aerial defense must survive own cleanup");}
        assert_eq!(g.player(p).mana_expended_this_turn,0);
        gl.expire_next_turn_effects(&mut g,PlayerId(1));
        for &id in &army {assert_eq!(g.card(id).has_keyword("Flying"),mode==1);}
        let late=add(&mut g,"Name:Late Cat\nTypes:Creature Cat\nPT:1/1",p,ZoneType::Battlefield);
        assert!(!g.card(late).has_keyword("Flying"),"the effect does not recollect later entrants");
        effect(&mut gl,&mut g,&mut agents,walker,"DB$ ChangeZone | Defined$ Self | Origin$ Battlefield | Destination$ Exile");
        for &id in &army {assert_eq!(g.card(id).has_keyword("Flying"),mode==1,"source leaving does not shorten resolved duration");}
        g.move_card(army[0],ZoneType::Exile,p);g.move_card(army[0],ZoneType::Battlefield,p);
        assert!(!g.card(army[0]).has_keyword("Flying"),"blink creates a new object");
        gl.expire_next_turn_effects(&mut g,p);
        for &id in &army {assert!(!g.card(id).has_keyword("Flying"));}
        assert_eq!(g.card(army[1]).power(),if mode==1{2}else{1},"permanent counters survive keyword expiration");
    }
    }).unwrap().join().unwrap();
}

#[path = "object_choices_repair.rs"]
mod object_choices_repair;

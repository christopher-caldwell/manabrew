//! Finding: myriad-attack-keyword-never-offers-or-creates-copies.
//! Exercise the generated trigger, public agent decisions, shared entry path,
//! combat damage and delayed cleanup, rather than calling a token effect alone.
use super::*;
use crate::agent::{PassAgent, PriorityActionSpace, TargetChoice};
use crate::player::actions::PlayerAction;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

const HERALD: &str = "Name:Herald of the Host\nManaCost:3 W W\nTypes:Creature Angel\nPT:4/4\nK:Flying\nK:Vigilance\nK:Myriad";
const GIADA: &str = "Name:Giada, Font of Hope\nManaCost:1 W\nTypes:Legendary Creature Angel\nPT:2/2\nK:Flying\nK:Vigilance\nK:ETBReplacement:Other:AddExtraCounter:Mandatory:Battlefield:Creature.Angel+YouCtrl+Other\nSVar:AddExtraCounter:DB$ PutCounter | ETB$ True | Defined$ ReplacedCard | CounterType$ P1P1 | CounterNum$ X\nSVar:X:Count$Valid Angel.YouCtrl";
const DOUBLE: &str = "Name:Multiplier\nTypes:Enchantment\nR:Event$ CreateToken | ActiveZones$ Battlefield | ValidToken$ Card.YouCtrl | ReplaceWith$ DoubleToken\nSVar:DoubleToken:DB$ ReplaceToken | Type$ Amount";

#[derive(Default)]
struct Decisions {
    answers: VecDeque<bool>,
    offers: Vec<String>,
    menus: Vec<(Vec<PlayerId>, Vec<CardId>)>,
    walker: bool,
}
struct Agent {
    attacker: CardId,
    decisions: Rc<RefCell<Decisions>>,
}
impl PlayerAgent for Agent {
    fn choose_land_or_spell(&mut self, _: PlayerId) -> Option<bool> {
        None
    }
    fn mulligan_decision(&mut self, _: PlayerId, _: &[CardId], _: u32) -> bool {
        true
    }
    fn choose_action(
        &mut self,
        _: PlayerId,
        _: Option<&PriorityActionSpace>,
        _: &mut dyn FnMut() -> PriorityActionSpace,
    ) -> PlayerAction {
        PlayerAction::PassPriority
    }
    fn choose_attackers(
        &mut self,
        _: PlayerId,
        _: &[CardId],
        _: &[combat::DefenderId],
    ) -> Vec<(CardId, combat::DefenderId)> {
        vec![(self.attacker, combat::DefenderId::Player(PlayerId(1)))]
    }
    fn choose_blockers(
        &mut self,
        _: PlayerId,
        _: &[CardId],
        _: &[CardId],
        _: Option<usize>,
    ) -> Vec<(CardId, CardId)> {
        vec![]
    }
    fn choose_targets_for(
        &mut self,
        sa: &mut SpellAbility,
        game: &GameState,
        pools: &[ManaPool],
    ) -> bool {
        crate::spellability::choose_targets_by_kind(self, sa, game, pools)
    }
    fn choose_target_player(
        &mut self,
        _: PlayerId,
        valid: &[PlayerId],
        _: Option<&SpellAbility>,
    ) -> Option<PlayerId> {
        valid.first().copied()
    }
    fn choose_target_card(
        &mut self,
        _: PlayerId,
        valid: &[CardId],
        _: Option<&SpellAbility>,
    ) -> Option<CardId> {
        valid.first().copied()
    }
    fn choose_target_any(
        &mut self,
        _: PlayerId,
        players: &[PlayerId],
        cards: &[CardId],
        _: Option<&SpellAbility>,
    ) -> TargetChoice {
        let mut decisions = self.decisions.borrow_mut();
        decisions.menus.push((players.to_vec(), cards.to_vec()));
        if decisions.walker && !cards.is_empty() {
            TargetChoice::Card(cards[0])
        } else {
            TargetChoice::Player(players[0])
        }
    }
    fn confirm_action(
        &mut self,
        _: PlayerId,
        mode: Option<&str>,
        message: &str,
        _: &[String],
        _: Option<CardId>,
        _: Option<crate::ability::api_type::ApiType>,
    ) -> bool {
        assert_eq!(mode, Some("Myriad"));
        let mut decisions = self.decisions.borrow_mut();
        decisions.offers.push(message.into());
        decisions.answers.pop_front().unwrap_or(true)
    }
}
fn add(g: &mut GameState, raw: &str, p: PlayerId) -> CardId {
    let rules = forge_carddb::parse_card_script(raw).unwrap();
    let id = g.create_card(Card::from_rules(&rules, p));
    g.move_card(id, ZoneType::Battlefield, p);
    id
}
fn fixture(
    script: &str,
) -> (
    GameState,
    GameLoop,
    Vec<Box<dyn PlayerAgent>>,
    CardId,
    Rc<RefCell<Decisions>>,
) {
    let mut g = GameState::new(&["A", "B", "C", "D"], 40);
    g.turn.phase = PhaseType::CombatDeclareAttackers;
    let id = add(&mut g, script, PlayerId(0));
    g.card_mut(id).summoning_sick = false;
    let decisions = Rc::new(RefCell::new(Decisions::default()));
    let agents = vec![
        Box::new(Agent {
            attacker: id,
            decisions: decisions.clone(),
        }) as Box<dyn PlayerAgent>,
        Box::new(PassAgent),
        Box::new(PassAgent),
        Box::new(PassAgent),
    ];
    let mut gl = GameLoop::new(4);
    gl.combat.attacking_player = Some(PlayerId(0));
    (g, gl, agents, id, decisions)
}
fn trigger(
    g: &mut GameState,
    gl: &mut GameLoop,
    agents: &mut [Box<dyn PlayerAgent>],
    id: CardId,
    defender: combat::DefenderId,
) {
    apply_continuous_effects(g);
    gl.trigger_handler.reset_active_triggers(g);
    gl.combat.add_attacker(id, defender);
    gl.trigger_handler.run_trigger(
        TriggerType::Attacks,
        RunParams {
            attacker: Some(id),
            defending_player: Some(defender.controlling_player(g)),
            attacked_player: defender.as_player(),
            attacked_card: match defender {
                combat::DefenderId::Permanent(id) => Some(id),
                _ => None,
            },
            num_attackers: Some(1),
            ..Default::default()
        },
        false,
    );
    gl.process_triggers(g, agents);
}
fn settle(g: &mut GameState, gl: &mut GameLoop, agents: &mut [Box<dyn PlayerAgent>]) {
    for _ in 0..30 {
        gl.process_triggers(g, agents);
        if g.stack.is_empty() {
            return;
        }
        gl.resolve_stack(g, agents);
    }
    panic!("recursive attack triggers or non-settling fixture");
}
fn tokens(g: &GameState) -> Vec<CardId> {
    g.cards
        .iter()
        .filter(|c| c.is_token && c.zone == ZoneType::Battlefield)
        .map(|c| c.id)
        .collect()
}
fn end_combat(g: &mut GameState, gl: &mut GameLoop, agents: &mut [Box<dyn PlayerAgent>]) {
    g.turn.phase = PhaseType::CombatEnd;
    gl.emit_phase_trigger(g, PhaseType::CombatEnd);
    settle(g, gl, agents);
}
fn large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn myriad_intrinsic_real_combat_offers_creates_deals_damage_and_exiles() {
    large_stack(|| {
        let (mut g, mut gl, mut agents, id, d) = fixture(HERALD);
        // An ordinary attack trigger fires once; copies entering attacking do not
        // generate fresh Attacks events, even though they copy this ability.
        let watcher=add(&mut g,"Name:Watcher\nTypes:Enchantment\nT:Mode$ Attacks | ValidCard$ Creature.YouCtrl | Execute$ Gain | TriggerZones$ Battlefield\nSVar:Gain:DB$ GainLife | LifeAmount$ 1",PlayerId(0));
        let _ = watcher;
        g.enable_journal();
        gl.step_combat(&mut g, &mut agents);
        assert_eq!(d.borrow().offers.len(), 2);
        assert_eq!(g.player(PlayerId(0)).life, 41);
        for p in 1..4 {
            assert_eq!(g.player(PlayerId(p)).life, 36);
        }
        assert!(tokens(&g).is_empty());
        assert_eq!(g.cards.iter().filter(|c| c.is_token).count(), 2);
        assert_eq!(g.card(id).zone, ZoneType::Battlefield);
        for token in g.cards.iter().filter(|c| c.is_token) {
            use crate::journal::JournalEvent;
            assert!(g.journal().iter().any(|e|matches!(e,JournalEvent::ZoneChange{card,from:ZoneType::None,to:ZoneType::Battlefield,..} if *card==token.id)));
            assert!(g.journal().iter().any(|e|matches!(e,JournalEvent::DamageToPlayer{source:Some(card),amount:4,combat:true,..} if *card==token.id)));
            assert!(g.journal().iter().any(|e|matches!(e,JournalEvent::ZoneChange{card,from:ZoneType::Battlefield,to:ZoneType::Exile,..} if *card==token.id)));
        }
    });
}

#[test]
fn myriad_optional_per_opponent_and_no_extra_opponent_control() {
    large_stack(|| {
        for (alive, answers, count, offers) in [
            (4, vec![false, true], 1, 2),
            (4, vec![false, false], 0, 2),
            (2, vec![], 0, 0),
        ] {
            let (mut g, mut gl, mut agents, id, d) = fixture(HERALD);
            d.borrow_mut().answers = answers.into();
            for p in alive..4 {
                g.player_mut(PlayerId(p)).life = 0;
                g.player_mut(PlayerId(p)).has_lost = true;
            }
            trigger(
                &mut g,
                &mut gl,
                &mut agents,
                id,
                combat::DefenderId::Player(PlayerId(1)),
            );
            assert_eq!(g.stack.len(), 1, "myriad itself triggers even in a duel");
            settle(&mut g, &mut gl, &mut agents);
            assert_eq!(d.borrow().offers.len(), offers);
            assert_eq!(tokens(&g).len(), count);
            if count == 1 {
                assert_eq!(
                    gl.combat.attackers[1].1,
                    combat::DefenderId::Player(PlayerId(3))
                );
            }
        }
    });
}

#[test]
fn myriad_granted_stacked_removed_and_temporary_instances() {
    large_stack(|| {
        for intrinsic in [false, true] {
            let script = if intrinsic {
                HERALD
            } else {
                "Name:Body\nTypes:Creature Angel\nPT:4/4"
            };
            let (mut g, mut gl, mut agents, id, d) = fixture(script);
            let grant=add(&mut g,"Name:Legion Loyalty\nTypes:Enchantment\nS:Mode$ Continuous | Affected$ Creature.YouCtrl | AddKeyword$ Myriad",PlayerId(0));
            let grant2=add(&mut g,"Name:Second grant\nTypes:Enchantment\nS:Mode$ Continuous | Affected$ Creature.YouCtrl | AddKeyword$ Myriad",PlayerId(0));
            trigger(
                &mut g,
                &mut gl,
                &mut agents,
                id,
                combat::DefenderId::Player(PlayerId(1)),
            );
            let instances = if intrinsic { 3 } else { 2 };
            assert_eq!(g.stack.len(), instances);
            // Removing grants after triggering cannot counter the independent abilities.
            g.move_card(grant, ZoneType::Graveyard, PlayerId(0));
            g.move_card(grant2, ZoneType::Graveyard, PlayerId(0));
            settle(&mut g, &mut gl, &mut agents);
            assert_eq!(d.borrow().offers.len(), instances * 2);
            assert_eq!(tokens(&g).len(), instances * 2);
            for token in tokens(&g) {
                assert_eq!(
                    g.card(token).has_keyword("Myriad"),
                    intrinsic,
                    "granted keywords are not copied"
                );
                assert_eq!(
                    g.card(token)
                        .triggers
                        .iter()
                        .filter(|t| t.execute == "TrigMyriad")
                        .count(),
                    usize::from(intrinsic)
                );
            }
            assert_eq!(
                g.card(id)
                    .triggers
                    .iter()
                    .filter(|t| t.execute == "TrigMyriad")
                    .count(),
                usize::from(intrinsic)
            );
        }
        let (mut g, mut gl, mut agents, id, d) = fixture("Name:Body\nTypes:Creature\nPT:2/2");
        g.card_mut(id).add_pump_keyword("Myriad");
        trigger(
            &mut g,
            &mut gl,
            &mut agents,
            id,
            combat::DefenderId::Player(PlayerId(1)),
        );
        settle(&mut g, &mut gl, &mut agents);
        assert_eq!(d.borrow().offers.len(), 2);
        g.card_mut(id).clear_pump_triggers();
        g.card_mut(id).clear_pump_keywords();
        assert!(!g
            .card(id)
            .triggers
            .iter()
            .any(|t| t.execute == "TrigMyriad"));
    });
}

#[test]
fn myriad_doubled_simultaneous_entries_use_prebatch_angels_and_copiable_values() {
    large_stack(|| {
        let (mut g, mut gl, mut agents, id, _) = fixture(HERALD);
        add(&mut g, GIADA, PlayerId(0));
        add(&mut g, DOUBLE, PlayerId(0));
        g.card_mut(id).add_counter(&CounterType::P1P1, 7);
        g.card_mut(id).power_modifier = 3;
        g.card_mut(id).add_pump_keyword("Haste");
        trigger(
            &mut g,
            &mut gl,
            &mut agents,
            id,
            combat::DefenderId::Player(PlayerId(1)),
        );
        settle(&mut g, &mut gl, &mut agents);
        let created = tokens(&g);
        assert_eq!(created.len(), 4);
        for token in created {
            let c = g.card(token);
            assert!(c.tapped);
            assert_eq!(c.base_power, Some(4));
            assert_eq!(
                c.counter_count(&CounterType::P1P1),
                2,
                "all four enter together with the two pre-existing Angels"
            );
            assert!(!c.has_keyword("Haste"));
            assert!(!c.attacked_this_turn);
            assert!(gl.combat.is_attacking(token));
        }
    });
}

#[test]
fn myriad_source_removal_uses_last_copiable_state_and_exile_tracks_token_identity() {
    large_stack(|| {
        let (mut g, mut gl, mut agents, id, _) = fixture(HERALD);
        trigger(
            &mut g,
            &mut gl,
            &mut agents,
            id,
            combat::DefenderId::Player(PlayerId(1)),
        );
        // A copy effect changes base values before departure; subsequent physical
        // reuse cannot change the pending ability's LKI.
        g.card_mut(id).base_power = Some(7);
        g.card_mut(id).base_toughness = Some(8);
        g.move_card(id, ZoneType::Graveyard, PlayerId(0));
        g.card_mut(id).base_power = Some(1);
        settle(&mut g, &mut gl, &mut agents);
        let created = tokens(&g);
        assert_eq!(created.len(), 2);
        assert!(created.iter().all(|id| g.card(*id).base_power == Some(7)));
        // This low-level physical-ID reuse models blinking a tracked object. Tokens
        // normally cease to exist off battlefield; a fresh object must still not
        // be exiled by an old delayed ability.
        let fresh = created[0];
        g.move_card(fresh, ZoneType::Exile, PlayerId(0));
        g.move_card(fresh, ZoneType::Battlefield, PlayerId(0));
        end_combat(&mut g, &mut gl, &mut agents);
        assert_eq!(g.card(fresh).zone, ZoneType::Battlefield);
        assert_ne!(g.card(created[1]).zone, ZoneType::Battlefield);
        assert_eq!(g.card(id).zone, ZoneType::Graveyard);
    });
}

#[test]
fn myriad_defending_planeswalker_excludes_its_controller_and_copies_choose_other_walkers() {
    large_stack(|| {
        let (mut g, mut gl, mut agents, id, d) = fixture(HERALD);
        let attacked = add(
            &mut g,
            "Name:Attacked Walker\nTypes:Planeswalker Test\nLoyalty:8",
            PlayerId(1),
        );
        let offered = add(
            &mut g,
            "Name:Other Walker\nTypes:Planeswalker Test\nLoyalty:8",
            PlayerId(2),
        );
        d.borrow_mut().walker = true;
        trigger(
            &mut g,
            &mut gl,
            &mut agents,
            id,
            combat::DefenderId::Permanent(attacked),
        );
        settle(&mut g, &mut gl, &mut agents);
        assert_eq!(d.borrow().offers.len(), 2);
        assert_eq!(d.borrow().menus.len(), 1);
        assert_eq!(d.borrow().menus[0], (vec![PlayerId(2)], vec![offered]));
        assert_eq!(
            gl.combat
                .attackers
                .iter()
                .filter(|(cid, _)| *cid != id)
                .map(|(_, d)| *d)
                .collect::<Vec<_>>(),
            vec![
                combat::DefenderId::Permanent(offered),
                combat::DefenderId::Player(PlayerId(3))
            ]
        );
    });
}

#[test]
fn myriad_copies_a_changed_permanent_and_lki_survives_restoration_on_departure() {
    large_stack(|| {
        let (mut g, mut gl, mut agents, id, _) =
            fixture("Name:Shapeshifter\nTypes:Creature Shapeshifter\nPT:1/1");
        let donor = add(&mut g, HERALD, PlayerId(0));
        let mut sa = crate::spellability::build_spell_ability(&g, id, "DB$ Clone", PlayerId(0));
        sa.target_chosen.target_card = Some(donor);
        gl.resolve_single_effect(&mut g, &mut agents, &sa, None);
        assert_eq!(g.card(id).card_name, "Herald of the Host");
        trigger(
            &mut g,
            &mut gl,
            &mut agents,
            id,
            combat::DefenderId::Player(PlayerId(1)),
        );
        g.move_card(id, ZoneType::Graveyard, PlayerId(0));
        assert_eq!(g.card(id).card_name, "Shapeshifter");
        settle(&mut g, &mut gl, &mut agents);
        assert_eq!(tokens(&g).len(), 2);
        for token in tokens(&g) {
            assert_eq!(g.card(token).card_name, "Herald of the Host");
            assert_eq!(g.card(token).base_power, Some(4));
        }
    });
}

#[test]
fn myriad_separate_instances_are_separate_entry_batches() {
    large_stack(|| {
        let (mut g, mut gl, mut agents, id, _) = fixture(HERALD);
        add(&mut g, GIADA, PlayerId(0));
        add(&mut g, DOUBLE, PlayerId(0));
        add(&mut g,"Name:Grant\nTypes:Enchantment\nS:Mode$ Continuous | Affected$ Creature.YouCtrl | AddKeyword$ Myriad",PlayerId(0));
        trigger(
            &mut g,
            &mut gl,
            &mut agents,
            id,
            combat::DefenderId::Player(PlayerId(1)),
        );
        assert_eq!(g.stack.len(), 2);
        settle(&mut g, &mut gl, &mut agents);
        let mut counters: Vec<_> = tokens(&g)
            .iter()
            .map(|id| g.card(*id).counter_count(&CounterType::P1P1))
            .collect();
        counters.sort();
        assert_eq!(counters, vec![2, 2, 2, 2, 6, 6, 6, 6]);
    });
}

#[test]
fn myriad_copy_does_not_make_continuously_granted_types_or_sizes_intrinsic() {
    large_stack(|| {
        let (mut g, mut gl, mut agents, id, _) =
            fixture("Name:Body\nTypes:Creature Spider\nPT:2/2\nK:Myriad");
        let grant=add(&mut g,"Name:Type and size grant\nTypes:Enchantment\nS:Mode$ Continuous | Affected$ Creature.YouCtrl | AddType$ Angel | AddPower$ 3 | AddToughness$ 3",PlayerId(0));
        trigger(
            &mut g,
            &mut gl,
            &mut agents,
            id,
            combat::DefenderId::Player(PlayerId(1)),
        );
        settle(&mut g, &mut gl, &mut agents);
        let created = tokens(&g);
        assert_eq!(created.len(), 2);
        assert!(created
            .iter()
            .all(|id| g.card(*id).power() == 5 && g.card(*id).type_line.has_subtype("Angel")));
        g.move_card(grant, ZoneType::Graveyard, PlayerId(0));
        apply_continuous_effects(&mut g);
        for id in created {
            assert_eq!(g.card(id).power(), 2);
            assert!(!g.card(id).type_line.has_subtype("Angel"));
            assert!(g.card(id).type_line.has_subtype("Spider"));
        }
    });
}

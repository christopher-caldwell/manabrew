// Supplied findings: cast-x-survives-noncast-zone-return and
// choice-dependent-protection-keeps-unresolved-type. Executed library regressions.
use super::*;
use crate::agent::{PriorityActionSpace, TargetChoice};
use crate::combat::DefenderId;
use crate::player::actions::PlayerAction;
use crate::spellability::SpellAbility;
use std::{cell::RefCell, rc::Rc};

struct ChoiceAgent {
    target: CardId,
    choice: String,
    calls: Rc<RefCell<Vec<Vec<String>>>>,
}
impl PlayerAgent for ChoiceAgent {
    fn choose_targets_for(
        &mut self,
        sa: &mut SpellAbility,
        g: &GameState,
        pools: &[crate::mana::ManaPool],
    ) -> bool {
        crate::spellability::choose_targets_by_kind(self, sa, g, pools)
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
        _: &[DefenderId],
    ) -> Vec<(CardId, DefenderId)> {
        vec![]
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
    fn choose_target_player(
        &mut self,
        _: PlayerId,
        valid: &[PlayerId],
        _: Option<&SpellAbility>,
    ) -> Option<PlayerId> {
        valid.last().copied()
    }
    fn choose_target_card(
        &mut self,
        _: PlayerId,
        valid: &[CardId],
        _: Option<&SpellAbility>,
    ) -> Option<CardId> {
        valid.iter().copied().find(|&id| id == self.target)
    }
    fn choose_target_any(
        &mut self,
        _: PlayerId,
        valid: &[PlayerId],
        _: &[CardId],
        _: Option<&SpellAbility>,
    ) -> TargetChoice {
        valid
            .last()
            .copied()
            .map(TargetChoice::Player)
            .unwrap_or(TargetChoice::None)
    }
    fn choose_sacrifice(
        &mut self,
        _: PlayerId,
        valid: &[CardId],
        _: Option<CardId>,
    ) -> Option<CardId> {
        valid.first().copied()
    }
    fn choose_land_or_spell(&mut self, _: PlayerId) -> Option<bool> {
        None
    }
    fn choose_number(
        &mut self,
        _: PlayerId,
        _: Option<CardId>,
        _: &str,
        _: Option<&str>,
        _: i32,
        max: i32,
    ) -> Option<i32> {
        Some(max)
    }
    fn choose_type_for_source(
        &mut self,
        _: PlayerId,
        category: &str,
        valid: &[String],
        _: Option<CardId>,
    ) -> Option<String> {
        assert_eq!(category, "Card");
        assert!(valid.contains(&self.choice));
        assert!(!valid.iter().any(|s| s == "CardType"));
        self.calls.borrow_mut().push(valid.to_vec());
        Some(self.choice.clone())
    }
    fn pay_mana_cost(
        &mut self,
        _: PlayerId,
        _: CardId,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: bool,
        _: bool,
        _: &[CardId],
        _: &[crate::agent::ManaAbilityOption],
        _: &[CardId],
        _: &[CardId],
        _: &crate::mana::ManaPool,
    ) -> crate::agent::ManaCostAction {
        crate::agent::ManaCostAction::Pay { auto: true }
    }
}
fn large(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}
fn paid_cast(
    g: &mut GameState,
    gl: &mut GameLoop,
    a: &mut [Box<dyn PlayerAgent>],
    id: CardId,
    x: i32,
) {
    gl.pool_mut(PlayerId(0))
        .add(forge_foundation::mana::ManaAtom::COLORLESS, 2 * x);
    let sa = crate::spellability::build_spell_ability_for_card_cast(g, id, PlayerId(0));
    let before = g.player(PlayerId(0)).mana_expended_this_turn;
    assert!(gl
        .cast_card_spell_ability(g, a, PlayerId(0), sa, false)
        .is_some());
    assert_eq!(
        g.stack.peek().unwrap().spell_ability.x_mana_cost_paid,
        x as u32
    );
    assert_eq!(
        g.player(PlayerId(0)).mana_expended_this_turn - before,
        2 * x
    );
    settle(gl, g, a);
    assert_eq!(g.card(id).counter_count(&crate::card::CounterType::P1P1), x);
}
fn auto_agents() -> Vec<Box<dyn PlayerAgent>> {
    (0..4)
        .map(|_| {
            Box::new(super::super::actual_mana_spend_regression::AutoAgent(
                true, false,
            )) as Box<dyn PlayerAgent>
        })
        .collect()
}

#[test]
fn cast_x_return_spent_counters_repeated_recovery_has_no_damage_resource() {
    large(|| {
        for x in [1, 2, 4] {
            let p = PlayerId(0);
            let mut g = GameState::new(&["A", "B", "C", "D"], 40);
            g.turn.phase = PhaseType::Main1;
            let id = add(&mut g, BALLISTA, p, ZoneType::Hand);
            let host = add(&mut g, "Name:Recovery\nTypes:Sorcery", p, ZoneType::Hand);
            let mut gl = GameLoop::new(4);
            let mut a = auto_agents();
            paid_cast(&mut g, &mut gl, &mut a, id, x);
            for left in (0..x).rev() {
                assert!(gl.get_activatable_abilities(&g, p, true).contains(&(id, 1)));
                let ab = g.card(id).activated_abilities[1].clone();
                assert!(gl.play_activated_ability_on_stack(&mut g, &mut a, p, id, &ab));
                g.check_state_based_actions();
                settle(&mut gl, &mut g, &mut a);
                assert_eq!(
                    g.card(id).counter_count(&crate::card::CounterType::P1P1),
                    left
                );
            }
            assert_eq!(
                g.player(p).life,
                40 - x,
                "paid counter damage resolves after source death"
            );
            assert_eq!(g.card(id).zone, ZoneType::Graveyard);
            for _ in 0..2 {
                let mut sa=crate::spellability::build_spell_ability(&g,host,"DB$ ChangeZone | ValidTgts$ Permanent | Origin$ Graveyard | Destination$ Battlefield",p);
                sa.target_chosen.target_card = Some(id);
                gl.resolve_single_effect(&mut g, &mut a, &sa, None);
                assert_eq!(
                    g.card(id).zone,
                    ZoneType::Battlefield,
                    "direct recovery must actually enter before testing counters"
                );
                assert_eq!(
                    g.card(id).counter_count(&crate::card::CounterType::P1P1),
                    0,
                    "prior X={x} must not survive recovery"
                );
                g.check_state_based_actions();
                assert_eq!(g.card(id).zone, ZoneType::Graveyard);
                assert!(!gl.get_activatable_abilities(&g, p, true).contains(&(id, 1)));
                assert_eq!(
                    g.player(p).life,
                    40 - x,
                    "recovery cannot fabricate another damage activation"
                );
            }
            // A new paid hand cast announces its own X after the same object died.
            g.move_card(id, ZoneType::Hand, p);
            paid_cast(&mut g, &mut gl, &mut a, id, 2);
        }
    });
}

#[test]
fn cast_x_blink_copy_and_independent_entry_controls() {
    large(|| {
        for (boost, extra) in [(false, 0), (true, 0), (false, 2)] {
            let p = PlayerId(0);
            let mut g = GameState::new(&["A", "B", "C", "D"], 40);
            g.turn.phase = PhaseType::Main1;
            if boost {
                add(&mut g,"Name:Anthem\nTypes:Enchantment\nS:Mode$ Continuous | Affected$ Creature.YouCtrl | AddPower$ 1 | AddToughness$ 1",p,ZoneType::Battlefield);
            }
            let id = add(&mut g, BALLISTA, p, ZoneType::Hand);
            let mut gl = GameLoop::new(4);
            let mut a = auto_agents();
            paid_cast(&mut g, &mut gl, &mut a, id, 4);
            let mut copy = crate::spellability::build_spell_ability(
                &g,
                id,
                "DB$ CopyPermanent | NumCopies$ 1",
                p,
            );
            copy.target_chosen.target_card = Some(id);
            gl.resolve_single_effect(&mut g, &mut a, &copy, None);
            let copied = g.cards.iter().find(|c| c.is_token).unwrap().id;
            assert_eq!(
                g.card(copied)
                    .counter_count(&crate::card::CounterType::P1P1),
                0,
                "copy does not copy cast X"
            );
            g.check_state_based_actions();
            assert_eq!(
                g.card(copied).zone,
                if boost {
                    ZoneType::Battlefield
                } else {
                    ZoneType::None
                }
            );
            g.move_card(id, ZoneType::Exile, p);
            if extra > 0 {
                g.card_mut(id).keywords.add("etbCounter:P1P1:2");
            }
            g.move_card(id, ZoneType::Battlefield, p);
            assert_eq!(
                g.card(id).counter_count(&crate::card::CounterType::P1P1),
                extra
            );
            g.check_state_based_actions();
            assert_eq!(
                g.card(id).zone,
                if boost || extra > 0 {
                    ZoneType::Battlefield
                } else {
                    ZoneType::Graveyard
                }
            );
        }
    });
}

const PIPPIN: &str = "Name:Pippin, Guard of the Citadel\nManaCost:W U\nTypes:Legendary Creature Halfling Soldier\nPT:2/2\nK:Vigilance\nK:Ward:1\nA:AB$ Protection | Cost$ T | ValidTgts$ Creature.YouCtrl+Other | Gains$ Choice | Choices$ CardType | SpellDescription$ Another target creature you control gains protection from the card type of your choice until end of turn.";
fn protection_fixture(
    choice: &str,
) -> (
    GameState,
    GameLoop,
    Vec<Box<dyn PlayerAgent>>,
    CardId,
    CardId,
    Rc<RefCell<Vec<Vec<String>>>>,
) {
    let p = PlayerId(0);
    let mut g = GameState::new(&["A", "B", "C", "D"], 40);
    let source = add(&mut g, PIPPIN, p, ZoneType::Battlefield);
    let target = add(
        &mut g,
        "Name:Protected flyer\nTypes:Creature Spirit\nPT:6/6\nK:Flying\nK:Trample",
        p,
        ZoneType::Battlefield,
    );
    g.card_mut(source).summoning_sick = false;
    let calls = Rc::new(RefCell::new(vec![]));
    let mut a = agents();
    a[0] = Box::new(ChoiceAgent {
        target,
        choice: choice.into(),
        calls: calls.clone(),
    });
    (g, GameLoop::new(4), a, source, target, calls)
}
fn activate(g: &mut GameState, gl: &mut GameLoop, a: &mut [Box<dyn PlayerAgent>], source: CardId) {
    let ab = g.card(source).activated_abilities[0].clone();
    assert!(gl
        .get_activatable_abilities(g, PlayerId(0), true)
        .contains(&(source, 0)));
    assert!(gl.play_activated_ability_on_stack(g, a, PlayerId(0), source, &ab));
    assert!(g.card(source).tapped);
}
#[test]
fn protection_card_type_choice_controls_blocking_targeting_damage_and_attachments() {
    large(|| {
        for choice in [
            "Creature",
            "Artifact",
            "Enchantment",
            "Instant",
            "Sorcery",
            "Land",
            "Planeswalker",
            "Battle",
            "Kindred",
        ] {
            let (mut g, mut gl, mut a, source, target, calls) = protection_fixture(choice);
            let flyer = add(
                &mut g,
                "Name:Blocker\nTypes:Creature Bird\nPT:2/2\nK:Flying",
                PlayerId(1),
                ZoneType::Battlefield,
            );
            let reach = add(
                &mut g,
                "Name:Reach\nTypes:Creature Spider\nPT:2/4\nK:Reach",
                PlayerId(1),
                ZoneType::Battlefield,
            );
            let ground = add(
                &mut g,
                "Name:Ground\nTypes:Creature Bear\nPT:2/2",
                PlayerId(1),
                ZoneType::Battlefield,
            );
            let equipment = add(
                &mut g,
                "Name:Equipment\nTypes:Artifact Equipment",
                PlayerId(0),
                ZoneType::Battlefield,
            );
            let aura=add(&mut g,"Name:Aura\nTypes:Enchantment Aura\nK:Enchant:Creature\nA:SP$ Attach | ValidTgts$ Creature",PlayerId(0),ZoneType::Battlefield);
            g.attach_to(equipment, target);
            g.attach_to(aura, target);
            activate(&mut g, &mut gl, &mut a, source);
            assert!(calls.borrow().is_empty(), "choice belongs at resolution");
            // An activated ability remains effective after its source leaves.
            g.move_card(source, ZoneType::Graveyard, PlayerId(0));
            settle(&mut gl, &mut g, &mut a);
            assert_eq!(calls.borrow().len(), 1);
            assert!(calls.borrow()[0].contains(&"Creature".into()));
            assert!(g.card(target).has_protection_from(&choice.to_lowercase()));
            assert!(!g.card(target).has_protection_from("cardtype"));
            assert_eq!(
                crate::combat::combat_util::can_creature_block(&g, flyer, target),
                choice != "Creature"
            );
            assert_eq!(
                crate::combat::combat_util::can_creature_block(&g, reach, target),
                choice != "Creature"
            );
            assert!(!crate::combat::combat_util::can_creature_block(
                &g, ground, target
            ));
            assert_eq!(
                crate::combat::filter_legal_blockers(&g, &[target], &[flyer, reach, ground]),
                if choice == "Creature" {
                    vec![]
                } else {
                    vec![flyer, reach]
                },
                "the production blocker menu must agree with resolved protection"
            );
            add(&mut g, "Name:Spirit restriction\nTypes:Enchantment\nS:Mode$ CantBlockBy | ValidAttacker$ Creature.YouCtrl | ValidBlocker$ Creature.nonSpirit", PlayerId(0), ZoneType::Battlefield);
            let spirit = add(
                &mut g,
                "Name:Spirit blocker\nTypes:Creature Spirit\nPT:1/1\nK:Flying",
                PlayerId(1),
                ZoneType::Battlefield,
            );
            assert_eq!(
                crate::combat::filter_legal_blockers(&g, &[target], &[flyer, spirit]),
                if choice == "Creature" {
                    vec![]
                } else {
                    vec![spirit]
                },
                "type protection must preserve independent Spirit restrictions"
            );
            for kind in [
                "Creature",
                "Artifact",
                "Enchantment",
                "Instant",
                "Sorcery",
                "Land",
                "Planeswalker",
                "Battle",
                "Kindred",
            ] {
                let raw = format!("Name:Damage source\nTypes:{kind}\nPT:2/10");
                let other = add(&mut g, &raw, PlayerId(1), ZoneType::Battlefield);
                let expected = choice == kind;
                assert_eq!(
                    g.card(target).is_protected_from(g.card(other)),
                    expected,
                    "choice={choice}, source={kind}"
                );
                assert_eq!(
                    crate::spellability::target_restrictions::can_be_targeted_by(
                        &g,
                        target,
                        PlayerId(1),
                        Some(other)
                    ),
                    !expected
                );
                let before = g.card(target).damage;
                let mut damage = crate::spellability::build_spell_ability(
                    &g,
                    other,
                    "DB$ DealDamage | NumDmg$ 1",
                    PlayerId(1),
                );
                damage.target_chosen.target_card = Some(target);
                gl.resolve_single_effect(&mut g, &mut a, &damage, None);
                assert_eq!(g.card(target).damage - before, if expected { 0 } else { 1 });
                g.card_mut(target).damage = 0;
            }
            g.check_state_based_actions();
            assert_eq!(
                g.card(equipment).attached_to,
                if choice == "Artifact" {
                    None
                } else {
                    Some(target)
                }
            );
            assert_eq!(
                g.card(aura).zone,
                if choice == "Enchantment" {
                    ZoneType::Graveyard
                } else {
                    ZoneType::Battlefield
                }
            );
            if choice == "Artifact" {
                let mut attach = crate::spellability::build_spell_ability(
                    &g,
                    equipment,
                    "DB$ Attach",
                    PlayerId(0),
                );
                attach.target_chosen.target_card = Some(target);
                gl.resolve_single_effect(&mut g, &mut a, &attach, None);
                assert_eq!(
                    g.card(equipment).attached_to,
                    None,
                    "non-target attachment cannot bypass protection"
                );
            }
            gl.step_cleanup(&mut g, &mut a);
            assert!(!g.card(target).has_protection_from(&choice.to_lowercase()));
            assert!(crate::combat::combat_util::can_creature_block(
                &g, spirit, target
            ));
        }
    });
}
#[test]
fn protection_invalidated_target_and_expiry_do_not_follow_new_objects() {
    large(|| {
        for before_resolution in [true, false] {
            let (mut g, mut gl, mut a, source, target, calls) = protection_fixture("Creature");
            activate(&mut g, &mut gl, &mut a, source);
            if !before_resolution {
                settle(&mut gl, &mut g, &mut a);
                assert!(g.card(target).has_protection_from("creature"));
            }
            g.move_card(target, ZoneType::Exile, PlayerId(0));
            g.move_card(target, ZoneType::Battlefield, PlayerId(0));
            settle(&mut gl, &mut g, &mut a);
            assert!(!g.card(target).has_protection_from("creature"));
            assert_eq!(calls.borrow().len(), usize::from(!before_resolution));
            gl.step_cleanup(&mut g, &mut a);
            assert!(!g.card(target).has_protection_from("creature"));
        }
    });
}
#[test]
fn protection_ordinary_color_control_needs_no_type_choice() {
    large(|| {
        let (mut g, mut gl, mut a, source, target, calls) = protection_fixture("Creature");
        let red = add(
            &mut g,
            "Name:Red\nManaCost:R\nTypes:Creature Goblin\nPT:2/2",
            PlayerId(1),
            ZoneType::Battlefield,
        );
        let mut sa = crate::spellability::build_spell_ability(
            &g,
            source,
            "DB$ Protection | Gains$ Protection from red",
            PlayerId(0),
        );
        sa.target_chosen.target_card = Some(target);
        gl.resolve_single_effect(&mut g, &mut a, &sa, None);
        assert!(calls.borrow().is_empty());
        assert!(g.card(target).is_protected_from(g.card(red)));
        gl.step_cleanup(&mut g, &mut a);
        assert!(!g.card(target).is_protected_from(g.card(red)));
        let color_choice = crate::spellability::build_spell_ability(
            &g,
            source,
            "DB$ Protection | Defined$ Self | Gains$ Choice | Choices$ AnyColor",
            PlayerId(0),
        );
        gl.resolve_single_effect(&mut g, &mut a, &color_choice, None);
        assert!(g.card(source).has_protection_from("white"));
        assert!(
            calls.borrow().is_empty(),
            "ordinary color choices keep their color interface"
        );
    });
}
#[test]
fn cast_x_zero_new_graveyard_cast_and_stack_copy_controls() {
    large(|| {
        let p = PlayerId(0);
        let mut g = GameState::new(&["A", "B", "C", "D"], 40);
        g.turn.phase = PhaseType::Main1;
        let id = add(&mut g, BALLISTA, p, ZoneType::Hand);
        let mut gl = GameLoop::new(4);
        let mut a = auto_agents();
        paid_cast(&mut g, &mut gl, &mut a, id, 0);
        g.check_state_based_actions();
        assert_eq!(g.card(id).zone, ZoneType::Graveyard);
        // Previously nonzero X cannot leak through a library move either.
        g.move_card(id, ZoneType::Hand, p);
        paid_cast(&mut g, &mut gl, &mut a, id, 4);
        g.put_on_bottom_of_library(id, p);
        assert!(!g.card(id).svars.contains_key("XPaid"));
        assert!(g.card(id).cast_sa.is_none());
        g.move_card(id, ZoneType::Graveyard, p);
        add(&mut g, PARAGON, p, ZoneType::Battlefield);
        gl.pool_mut(p)
            .add(forge_foundation::mana::ManaAtom::COLORLESS, 6);
        let sa = crate::spellability::build_spell_ability_for_card_cast(&g, id, p);
        assert!(gl
            .cast_card_spell_ability(&mut g, &mut a, p, sa, false)
            .is_some());
        let original = g.stack.peek().unwrap().spell_ability.clone();
        assert_eq!(
            original.x_mana_cost_paid, 1,
            "graveyard permission caps newly announced X"
        );
        let copied =
            crate::card::card_factory::copy_spell_ability_and_possibly_host(&mut g, &original, p);
        let copied_id = copied.source.unwrap();
        assert_eq!(copied.x_mana_cost_paid, 1);
        assert_eq!(
            g.card(copied_id).svars.get("XPaid").map(String::as_str),
            Some("1")
        );
        // A copied permanent spell preserves X as it becomes a token.
        let prototype = crate::ability::effects::copy_permanent_effect::get_proto_type(
            &copied,
            g.card(copied_id),
            p,
        );
        let token = g.create_card(prototype);
        g.move_card(token, ZoneType::Battlefield, p);
        assert_eq!(
            g.card(token).counter_count(&crate::card::CounterType::P1P1),
            1
        );
        // Pending spell ability owns X even when its old source leaves stack.
        g.move_card(id, ZoneType::Exile, p);
        assert_eq!(g.stack.peek().unwrap().spell_ability.x_mana_cost_paid, 1);
        assert_eq!(crate::svar::resolve_numeric_value(&g, &original, "X", 0), 1);
    });
}

#[test]
fn cast_x_pending_spell_damage_retains_x_after_source_changes_zone() {
    large(|| {
        let p = PlayerId(0);
        let mut g = GameState::new(&["A", "B", "C", "D"], 40);
        g.turn.phase = PhaseType::Main1;
        // A real paid X spell changes its own zone before its damage subability.
        let source = add(&mut g, "Name:Self-exiling X fixture\nManaCost:X\nTypes:Sorcery\nA:SP$ ChangeZone | Defined$ Self | Origin$ Stack | Destination$ Exile | SubAbility$ Hit\nSVar:Hit:DB$ DealDamage | ValidTgts$ Player | NumDmg$ X\nSVar:X:Count$xPaid", p, ZoneType::Hand);
        let mut gl = GameLoop::new(4);
        gl.pool_mut(p)
            .add(forge_foundation::mana::ManaAtom::COLORLESS, 3);
        let mut a = agents();
        a[0] = Box::new(ChoiceAgent {
            target: source,
            choice: "Creature".into(),
            calls: Rc::new(RefCell::new(vec![])),
        });
        let sa = crate::spellability::build_spell_ability_for_card_cast(&g, source, p);
        assert!(gl
            .cast_card_spell_ability(&mut g, &mut a, p, sa, false)
            .is_some());
        assert_eq!(g.stack.peek().unwrap().spell_ability.x_mana_cost_paid, 3);
        assert_eq!(g.player(p).mana_expended_this_turn, 3);
        settle(&mut gl, &mut g, &mut a);
        assert_eq!(g.card(source).zone, ZoneType::Exile);
        assert!(!g.card(source).svars.contains_key("XPaid"));
        assert_eq!(
            g.player(PlayerId(3)).life,
            37,
            "pending damage retains announced X across its source's zone change"
        );
    });
}

#[test]
fn protection_existing_double_block_damage_and_already_open_attack_controls() {
    large(|| {
        for choice in ["Creature", "Artifact"] {
            for blocked in [true, false] {
                let (mut g, mut gl, mut a, source, target, _) = protection_fixture(choice);
                let mut combat = crate::combat::CombatState::new();
                combat.add_attacker(target, DefenderId::Player(PlayerId(1)));
                if blocked {
                    for _ in 0..2 {
                        let blocker = add(
                            &mut g,
                            "Name:Flyer\nTypes:Creature Bird\nPT:2/2\nK:Flying",
                            PlayerId(1),
                            ZoneType::Battlefield,
                        );
                        combat.add_blocker(target, blocker);
                    }
                }
                activate(&mut g, &mut gl, &mut a, source);
                settle(&mut gl, &mut g, &mut a);
                combat.resolve_damage_step(&mut g, &mut a, false, &Default::default());
                assert_eq!(
                    g.card(target).damage,
                    if blocked && choice != "Creature" {
                        4
                    } else {
                        0
                    }
                );
                assert_eq!(
                    g.player(PlayerId(1)).life,
                    if blocked { 38 } else { 34 },
                    "trample and an already open attack are separate from prevention"
                );
            }
        }
    });
}

#[test]
fn protection_mass_choice_is_once_and_no_legal_target_does_not_offer_activation() {
    large(|| {
        let (mut g, mut gl, mut a, source, target, calls) = protection_fixture("Instant");
        let sa = crate::spellability::build_spell_ability(
            &g,
            source,
            "DB$ ProtectionAll | ValidCards$ Creature.YouCtrl | Gains$ Choice | Choices$ CardType",
            PlayerId(0),
        );
        gl.resolve_single_effect(&mut g, &mut a, &sa, None);
        assert_eq!(calls.borrow().len(), 1);
        assert!(g.card(source).has_protection_from("instant"));
        assert!(g.card(target).has_protection_from("instant"));
        gl.step_cleanup(&mut g, &mut a);
        assert!(!g.card(target).has_protection_from("instant"));
        g.move_card(target, ZoneType::Exile, PlayerId(0));
        assert!(!gl
            .get_activatable_abilities(&g, PlayerId(0), true)
            .contains(&(source, 0)));
        assert_eq!(calls.borrow().len(), 1);
    });
}

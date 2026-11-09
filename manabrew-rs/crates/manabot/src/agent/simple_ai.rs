use std::collections::{HashMap, HashSet, VecDeque};

use manabrew_agent_interface::game_view_dto::{
    CardDto, CardView, GameViewDto, StepKind, TargetingIntent, ZoneKind,
};
use manabrew_agent_interface::prompt::*;

use super::BotAgent;

mod payment;
mod plan;
mod roles;
mod threat;
use roles::Roles;

/// How many recent prompts to remember when detecting a stuck loop.
const LOOP_WINDOW: usize = 6;

fn bot_warn(msg: &str) {
    #[cfg(target_arch = "wasm32")]
    if web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item("manabrew.debugPrompts").ok().flatten())
        .as_deref()
        == Some("1")
    {
        web_sys::console::warn_1(&format!("[wasm-bot] {msg}").into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    tracing::debug!(target: "wasm-bot", "{msg}");
}

/// Baseline AI: casts spells when possible, otherwise passes priority, with a
/// memoized anti-loop heuristic so a stuck `ChooseAction` doesn't repeat the
/// same non-pass choice indefinitely.
#[derive(Default)]
pub struct SimpleAi {
    recent_prompts: VecDeque<String>,
    last_attack_declaration: Vec<(String, String)>,
    failed_attack_targets: HashSet<String>,
    view: Option<GameViewDto>,
    pending_view: Option<String>,
    card_locations: HashMap<String, (usize, usize)>,
    turn: Option<u32>,
    attempted_actions: HashSet<String>,
    payment_attempt: Option<String>,
    payment_actions: HashSet<String>,
    payment_state: Option<String>,
    pending_mana_color: Option<manabrew_protocol::game::ManaColor>,
    awaiting_mana_color: bool,
    auto_payment_active: bool,
    selected_mana_source: Option<String>,
    has_command_cards: bool,
    pending_hand_refill_source: Option<String>,
    last_lifelink_combo_progress: Option<i32>,
    pending_lifelink_combo_cost: Option<String>,
    last_tutor_sacrifice_state: Option<String>,
    known_tutor_targets: Option<HashSet<String>>,
    pending_voluntary_cast: Option<String>,
    /// Double-faced cards seen on their front face with an entry ability.
    /// An exile-and-return brings such a card back on that face.
    front_face_entry: HashSet<String>,
    /// Target chosen for a pending optional exile-and-return, by source card.
    pending_reuse_target: Option<(String, String)>,
    /// Source whose repeated "pay any amount of life" rounds this player has
    /// already paid into this turn. Views may not refresh between rounds.
    paid_life_round: Option<String>,
    /// Plays the bot started and then cancelled at target selection, by card, with the
    /// board they were judged against. The same cast is not started again until it changes.
    cancelled_plays: std::collections::BTreeMap<String, u64>,
    /// The cast the bot last chose, with the stack size then. If that card is still in hand at
    /// the next action prompt with nothing added to the stack, the play failed.
    last_cast_pick: Option<(String, usize)>,
    /// The mana cost the turn plan keeps open at this prompt, if any (see `plan.rs`).
    reserve: Option<String>,
}

const KNOWN_TYPES: [&str; 8] = ["creature", "instant", "sorcery", "enchantment", "artifact", "planeswalker", "land", "battle"];

struct Combatant {
    id: String,
    power: i32,
    lethal: i32,
    value: i32,
    deathtouch: bool,
    first_strike: bool,
    double_strike: bool,
    indestructible: bool,
    trample: bool,
}

impl SimpleAi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe_lazy(&mut self, view_json: String) {
        self.pending_view = Some(view_json);
    }

    fn ensure_view(&mut self) {
        if let Some(json) = self.pending_view.take() {
            match serde_json::from_str::<GameViewDto>(&json) {
                Ok(view) => self.observe(view),
                Err(error) => bot_warn(&format!("invalid game view: {error}")),
            }
        }
    }

    fn needs_view(prompt: &PromptInput) -> bool {
        match prompt {
            PromptInput::ChooseAction(input) => input.actions.iter().any(|action| {
                !matches!(
                    &action.kind,
                    AvailableActionKind::UndoMana { .. }
                        | AvailableActionKind::ActivateAbility(ActivatableAbilityInfo {
                            is_mana_ability: true,
                            ..
                        })
                )
            }),
            PromptInput::PayManaCost(_)
            | PromptInput::RevealCards(_)
            | PromptInput::DiceRolled(_)
            | PromptInput::Reorder(_)
            | PromptInput::GameOver(_) => false,
            _ => true,
        }
    }

    /// Detects infinite response loops from the bot
    /// to avoid getting it stuck
    fn looping_on(&mut self, signature: String) -> bool {
        let seen = self.recent_prompts.contains(&signature);
        if seen {
            bot_warn(&format!(
                "loop-breaker engaged on repeated prompt: {signature}"
            ));
        }
        self.remember(signature);
        seen
    }

    fn looping_on_consecutive(&mut self, signature: String) -> bool {
        let consecutive = self.recent_prompts.back() == Some(&signature);
        if consecutive {
            bot_warn(&format!(
                "loop-breaker engaged on consecutive prompt: {signature}"
            ));
        }
        self.remember(signature);
        consecutive
    }

    fn remember(&mut self, signature: String) {
        self.recent_prompts.push_back(signature);
        while self.recent_prompts.len() > LOOP_WINDOW {
            self.recent_prompts.pop_front();
        }
    }

    fn fail_attack_target(&mut self, card_id: &str) {
        if let Some((_, target_id)) = self
            .last_attack_declaration
            .iter()
            .find(|(a, _)| a == card_id)
        {
            self.failed_attack_targets.insert(target_id.clone());
        }
    }

    fn card_zone(&self, id: &str) -> Option<ZoneKind> {
        let (zone_index, _) = *self.card_locations.get(id)?;
        self.view
            .as_ref()?
            .zones
            .get(zone_index)
            .map(|zone| zone.zone)
    }

    fn card(&self, id: &str) -> Option<&CardDto> {
        let (zone_index, card_index) = *self.card_locations.get(id)?;
        match self
            .view
            .as_ref()?
            .zones
            .get(zone_index)?
            .cards
            .get(card_index)?
        {
            CardView::Visible(card) => Some(card),
            CardView::Hidden { .. } => None,
        }
    }

    fn card_value(card: &CardDto) -> i32 {
        let power = card
            .power
            .as_deref()
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        let toughness = card
            .toughness
            .as_deref()
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        let keyword_value = card
            .keywords
            .iter()
            .map(|keyword| match keyword.to_ascii_lowercase().as_str() {
                "flying" => power * 2,
                "double strike" => power * 3,
                "first strike" | "lifelink" | "trample" => power,
                "deathtouch" => 4,
                "hexproof" | "indestructible" => 3,
                "vigilance" | "menace" => 2,
                _ => 0,
            })
            .sum::<i32>();
        power * 3 + toughness * 2 + card.cmc + keyword_value
    }

    /// The card's own entry trigger ("When ~ enters"), not "Whenever another ... enters".
    fn has_entry_ability(text: &str) -> bool {
        text.lines().any(|line| {
            let line = line.trim().to_ascii_lowercase();
            line.starts_with("when ") && line.contains(" enters")
        })
    }

    /// Value of exiling one of the player's own permanents and returning it at
    /// once. `None` when the re-entry gains nothing: a token (it ceases to
    /// exist), an opponent's object, or a creature with no entry ability that
    /// nothing on the stack threatens.
    fn reentry_value(&self, card: &CardDto, player_id: &str) -> Option<i32> {
        if card.identity.is_token || card.controller_id != player_id {
            return None;
        }
        let threatened = self.view.as_ref().is_some_and(|view| {
            view.stack.iter().filter(|item| item.controller_id != player_id).any(|item| {
                let effect = item.source_ability_text.as_deref().filter(|s| !s.is_empty()).unwrap_or(&item.text).to_ascii_lowercase();
                let destructive = ["destroy", "exile", "sacrifice", "damage", "return", "-x/-x", "-1/-1"].iter().any(|word| effect.contains(word));
                let wide = ["all creature", "each creature", "all permanent", "each permanent", "each nonland"].iter().any(|word| effect.contains(word));
                item.targets.iter().any(|target| target.id == card.id) || (destructive && wide)
            })
        });
        let entry = Self::has_entry_ability(&card.text)
            || (card.is_double_faced && card.is_transformed && self.front_face_entry.contains(&card.id));
        if !entry && !threatened {
            return None;
        }
        let mut value = 0;
        if entry {
            value += 100 + card.cmc * 10;
        }
        if threatened {
            value += 200;
        }
        if card.is_attacking && !threatened {
            value -= 150;
        }
        value -= card.counters.values().sum::<u32>() as i32 * 5;
        if !card.attachment_ids.is_empty() {
            value -= 20;
        }
        Some(value)
    }

    /// `{1}{B}`-style cost against the player's untapped sources.
    fn cost_affordable(&self, cost: &str, player_id: &str) -> bool {
        let (colors, any, total) = self.mana_sources(player_id);
        let mut generic = 0;
        let mut pips = [0i32; 5];
        for symbol in cost.split('{').filter_map(|part| part.split('}').next()).filter(|s| !s.is_empty()) {
            if let Ok(amount) = symbol.parse::<i32>() {
                generic += amount;
            } else if let Some(i) = "WUBRG".find(symbol) {
                pips[i] += 1;
            } else {
                return false;
            }
        }
        let short: i32 = (0..5).map(|i| (pips[i] - colors[i]).max(0)).sum();
        generic + pips.iter().sum::<i32>() <= total && short <= any
    }

    /// Public-information finisher check: a spell making each player lose half
    /// their life, with an opponent-life-loss doubler on the player's own turn,
    /// that would leave every remaining opponent at 0 while the player survives.
    fn mass_life_loss_wins(&self, card: &CardDto, player_id: &str) -> bool {
        let text = card.text.to_ascii_lowercase();
        if !text.contains("each player loses half their life") {
            return false;
        }
        let Some(view) = self.view.as_ref() else { return false };
        let doubled = view.active_player_id == player_id
            && self.battlefield(player_id).any(|source| {
                let text = source.text.to_ascii_lowercase();
                text.contains("if an opponent would lose life during your turn") && text.contains("twice that much")
            });
        let half = |life: i32| (life.max(0) + 1) / 2;
        let opponents: Vec<_> = view.players.iter()
            .filter(|p| p.id != player_id && p.status == manabrew_protocol::game::PlayerStatus::Playing)
            .collect();
        let own = view.players.iter().find(|p| p.id == player_id).map_or(0, |p| p.life);
        !opponents.is_empty()
            && opponents.iter().all(|p| half(p.life) * if doubled { 2 } else { 1 } >= p.life)
            && own - half(own) > 0
    }

    fn action_key(action: &AvailableAction) -> String {
        match &action.kind {
            AvailableActionKind::Cast { card_id, mode, .. } => format!("cast:{card_id}:{mode:?}"),
            AvailableActionKind::ActivateAbility(info) => {
                format!("ability:{}:{}", info.card_id, info.description)
            }
            AvailableActionKind::UndoMana { card_id } => format!("undo:{card_id}"),
        }
    }

    fn prefers_creatures(&self, player_id: &str) -> bool {
        self.view.as_ref().is_some_and(|view| {
            view.zones
                .iter()
                .filter(|zone| matches!(zone.zone, ZoneKind::Command | ZoneKind::Battlefield))
                .flat_map(|zone| &zone.cards)
                .any(|card| {
                    matches!(
                        card,
                        CardView::Visible(card)
                            if card.owner_id == player_id
                                && card.text.to_ascii_lowercase().contains("creature spells")
                    )
                })
        })
    }

    fn land_colors(card: &CardDto) -> HashSet<char> {
        let mut colors = HashSet::new();
        for subtype in &card.subtypes {
            match subtype.as_str() {
                "Plains" => colors.insert('W'),
                "Island" => colors.insert('U'),
                "Swamp" => colors.insert('B'),
                "Mountain" => colors.insert('R'),
                "Forest" => colors.insert('G'),
                _ => false,
            };
        }
        for segment in card.text.split("Add ").skip(1) {
            for symbol in segment.split(['.', '\n']).next().unwrap_or("").chars() {
                if "WUBRG".contains(symbol) {
                    colors.insert(symbol);
                }
            }
        }
        colors
    }

    fn missing_colors(&self, player_id: &str) -> HashSet<char> {
        let have = self
            .battlefield(player_id)
            .filter(|card| card.types.iter().any(|ty| ty == "Land"))
            .flat_map(Self::land_colors)
            .collect::<HashSet<_>>();
        self.view
            .iter()
            .flat_map(|view| &view.zones)
            .filter(|zone| {
                matches!(zone.zone, ZoneKind::Hand | ZoneKind::Command)
                    && zone.owner_id == player_id
            })
            .flat_map(|zone| &zone.cards)
            .filter_map(|card| match card {
                CardView::Visible(card) => Some(card),
                CardView::Hidden { .. } => None,
            })
            .flat_map(|card| card.mana_cost.chars().filter(|c| "WUBRG".contains(*c)))
            .filter(|color| !have.contains(color))
            .collect()
    }

    fn is_land_play(&self, action: &AvailableAction) -> bool {
        match &action.kind {
            AvailableActionKind::Cast { card_id, label, .. } => {
                label.starts_with("Play ")
                    || self
                        .card(card_id)
                        .is_some_and(|card| card.types.iter().any(|ty| ty == "Land"))
            }
            _ => false,
        }
    }

    fn opponent_creatures(&self, player_id: &str) -> Vec<&CardDto> {
        self.view
            .iter()
            .flat_map(|view| &view.zones)
            .filter(|zone| zone.zone == ZoneKind::Battlefield && zone.owner_id != player_id)
            .flat_map(|zone| &zone.cards)
            .filter_map(|card| match card {
                CardView::Visible(card) if card.types.iter().any(|ty| ty == "Creature") => {
                    Some(card)
                }
                _ => None,
            })
            .collect()
    }

    /// Life a mana ability asks for ("{T}, Pay 1 life: Add one mana of any color.").
    fn life_cost_of_mana(text: &str) -> Option<i32> {
        text.split('\n').filter(|line| line.contains("Add ")).find_map(|line| {
            let cost = line.split(':').next()?.to_ascii_lowercase();
            let rest = cost.split("pay ").nth(1)?;
            let mut words = rest.split_whitespace();
            let amount = words.next()?.parse::<i32>().ok()?;
            words.next().filter(|word| word.trim_matches(|c: char| !c.is_alphabetic()) == "life")?;
            Some(amount)
        })
    }

    fn mana_output(card: &CardDto) -> Option<([i32; 5], i32, i32)> {
        let mut colors = [0i32; 5];
        let mut any = 0;
        let mut amount = 0;
        for subtype in &card.subtypes {
            if let Some(i) = ["Plains", "Island", "Swamp", "Mountain", "Forest"]
                .iter()
                .position(|basic| basic == subtype)
            {
                colors[i] += 1;
                amount = amount.max(1);
            }
        }
        for segment in card.text.split("Add ").skip(1) {
            let clause = segment.split(['.', '\n']).next().unwrap_or("");
            let lower = clause.to_ascii_lowercase();
            // Alternative colors from one activation do not create multiple
            // mana. Count the largest offered bundle, not all alternatives.
            let symbols = clause.split(" or ").map(|option| option.matches('{').count() as i32).max().unwrap_or(0);
            let mut produced = 0;
            for symbol in clause.chars() {
                if let Some(i) = "WUBRG".find(symbol) {
                    colors[i] += 1;
                    produced = produced.max(1);
                }
            }
            if lower.contains("any color")
                || lower.contains("any type")
                || lower.contains("any one color")
            {
                let count = if lower.starts_with("two") {
                    2
                } else if lower.starts_with("three") {
                    3
                } else {
                    1
                };
                any += count;
                produced = produced.max(count);
            }
            amount = amount.max(produced.max(symbols));
        }
        // Basic land subtype and its printed mana ability describe the same
        // activation; do not count the same source twice for one color.
        for count in &mut colors { *count = (*count).min(amount); }
        any = any.min(amount);
        (amount > 0).then_some((colors, any, amount))
    }

    fn mana_sources(&self, player_id: &str) -> ([i32; 5], i32, i32) {
        let mut colors = [0i32; 5];
        let mut any = 0;
        let mut total = 0;
        let life = self.view.as_ref().and_then(|v| v.players.iter().find(|p| p.id == player_id)).map_or(i32::MAX, |p| p.life);
        for card in self.battlefield(player_id).filter(|card| !card.tapped) {
            let creature = card.types.iter().any(|ty| ty == "Creature");
            if creature && (card.summoning_sick || !card.text.contains("{T}: Add")) {
                continue;
            }
            // A source that costs life is spent only above that life, as the payment prompt
            // decides; counting it would start a cast the payment then refuses.
            if Self::life_cost_of_mana(&card.text).is_some_and(|cost| life <= cost) {
                continue;
            }
            if let Some((produced, wild, amount)) = Self::mana_output(card) {
                total += amount;
                any += wild;
                for i in 0..5 {
                    colors[i] += produced[i];
                }
            }
        }
        if let Some(player) = self.view.as_ref().and_then(|v| v.players.iter().find(|p| p.id == player_id)) {
            use manabrew_protocol::game::ManaColor;
            for (color, &amount) in &player.mana_pool {
                let amount = amount as i32;
                total += amount;
                let i = match color { ManaColor::White => Some(0), ManaColor::Blue => Some(1), ManaColor::Black => Some(2), ManaColor::Red => Some(3), ManaColor::Green => Some(4), ManaColor::Colorless => None };
                if let Some(i) = i { colors[i] += amount; }
            }
        }
        (colors, any, total)
    }

    fn affordable(&self, card: &CardDto, player_id: &str) -> bool {
        let cost = card
            .effective_mana_cost
            .as_deref()
            .unwrap_or(card.mana_cost.as_str());
        if cost.contains('/') || cost.is_empty() {
            return true;
        }
        let (mut colors, any, mut total) = self.mana_sources(player_id);
        if card.keywords.iter().any(|kw| kw.eq_ignore_ascii_case("Convoke")) {
            for creature in self.battlefield(player_id).filter(|c| !c.tapped && c.types.iter().any(|t| t == "Creature")) {
                // A source already counted by mana_sources cannot be tapped twice.
                let already_counted = !creature.summoning_sick && creature.text.contains("{T}: Add") && Self::mana_output(creature).is_some();
                if already_counted { continue; }
                total += 1;
                for color in creature.color.chars() { if let Some(i) = "WUBRG".find(color) { colors[i] += 1; } }
            }
        }
        let tax = card.commander_tax.unwrap_or(0);
        if card.cmc + tax > total {
            return false;
        }
        let mut pips = [0i32; 5];
        for symbol in cost.chars() {
            if let Some(i) = "WUBRG".find(symbol) {
                pips[i] += 1;
            }
        }
        let short: i32 = (0..5).map(|i| (pips[i] - colors[i]).max(0)).sum();
        if short > any {
            return false;
        }
        // The counts above add a land's colours up, so a land making green or blue pays for both.
        // Assigning each pip its own source says whether the cost can be paid at all.
        let convoke = card.keywords.iter().any(|kw| kw.eq_ignore_ascii_case("Convoke"));
        match (payment::cost_pips(cost), self.mana_supplies(player_id)) {
            (Some(pips), Some(supplies)) if !convoke => {
                let pips: Vec<_> = pips.into_iter().filter(|pip| !pip.contains(&manabrew_protocol::game::ManaColor::Colorless)).collect();
                payment::coverable(&pips, &supplies)
            }
            _ => true,
        }
    }

    /// The colours each untapped source and each floating mana can make, one entry per mana, for
    /// the assignment of coloured pips. `None` when a source makes several colours at once
    /// ("Add {W}{U}"), which this does not model; the caller then keeps its own count.
    fn mana_supplies(&self, player_id: &str) -> Option<Vec<Vec<manabrew_protocol::game::ManaColor>>> {
        use manabrew_protocol::game::ManaColor::{Black, Blue, Green, Red, White};
        const COLORS: [manabrew_protocol::game::ManaColor; 5] = [White, Blue, Black, Red, Green];
        let mut supplies = Vec::new();
        let life = self.view.as_ref().and_then(|v| v.players.iter().find(|p| p.id == player_id)).map_or(i32::MAX, |p| p.life);
        for card in self.battlefield(player_id).filter(|card| !card.tapped) {
            let creature = card.types.iter().any(|ty| ty == "Creature");
            if creature && (card.summoning_sick || !card.text.contains("{T}: Add")) {
                continue;
            }
            if Self::life_cost_of_mana(&card.text).is_some_and(|cost| life <= cost) {
                continue;
            }
            let Some((produced, wild, amount)) = Self::mana_output(card) else { continue };
            for clause in card.text.split("Add ").skip(1) {
                let clause = clause.split(['.', '\n']).next().unwrap_or("");
                let symbols: std::collections::BTreeSet<&str> = clause.split('{').skip(1)
                    .filter_map(|part| part.split('}').next()).filter(|symbol| matches!(*symbol, "W" | "U" | "B" | "R" | "G")).collect();
                if symbols.len() > 1 && !clause.contains(" or ") {
                    return None;
                }
            }
            let options: Vec<_> = (0..5).filter(|&i| produced[i] > 0 || wild > 0).map(|i| COLORS[i]).collect();
            if !options.is_empty() {
                supplies.extend(std::iter::repeat(options).take(amount.max(1) as usize));
            }
        }
        if let Some(player) = self.view.as_ref().and_then(|v| v.players.iter().find(|p| p.id == player_id)) {
            for (color, &amount) in &player.mana_pool {
                supplies.extend(std::iter::repeat(vec![*color]).take(amount as usize));
            }
        }
        Some(supplies)
    }

    fn roles(card: &CardDto) -> Roles {
        roles::lookup(&card.identity.name)
    }

    fn opponent_permanents(&self, player_id: &str) -> usize {
        self.view
            .iter()
            .flat_map(|view| &view.zones)
            .filter(|zone| zone.zone == ZoneKind::Battlefield && zone.owner_id != player_id)
            .flat_map(|zone| &zone.cards)
            .filter(|card| match card {
                CardView::Visible(card) => !card.types.iter().any(|ty| ty == "Land"),
                CardView::Hidden { .. } => false,
            })
            .count()
    }

    /// A fingerprint of what a cast's targets are chosen from: every permanent and stack object.
    fn board_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(view) = &self.view {
            for zone in &view.zones {
                if zone.zone == ZoneKind::Battlefield {
                    for card in &zone.cards {
                        if let CardView::Visible(card) = card {
                            card.id.hash(&mut hasher);
                        }
                    }
                }
            }
            for item in &view.stack {
                item.id.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// The bot started `source`'s play and cancelled it at target selection.
    fn note_cancelled_play(&mut self, source_id: &str) {
        let signature = self.board_signature();
        self.cancelled_plays.insert(source_id.to_string(), signature);
    }

    /// An opponent's spell or ability that counters, destroys, exiles or bounces something a
    /// third seat controls. Stopping it would only protect that object for its owner, so it is
    /// not a target for our counter.
    fn answers_another_opponent(&self, item: &manabrew_agent_interface::game_view_dto::StackObjectDto, player_id: &str) -> bool {
        let Some(view) = &self.view else { return false };
        item.controller_id != player_id && item.targets.iter().any(|t| {
            let controller = match t.kind {
                TargetKind::Spell => view.stack.iter().find(|other| other.id == t.id).map(|other| other.controller_id.as_str()),
                TargetKind::Card => self.card(&t.id).map(|card| card.controller_id.as_str()),
                _ => None,
            };
            matches!(t.intent, Some(TargetingIntent::Counter | TargetingIntent::Destroy | TargetingIntent::Exile | TargetingIntent::Bounce))
                && controller.is_some_and(|c| c != player_id && c != item.controller_id)
        })
    }

    /// An opposing spell that "counter target ..." text could legally take, that no answer of ours
    /// already covers, and that matters enough to spend a counterspell on (`threat::COUNTER_MIN`).
    /// Without one, a counterspell would be started and cancelled, or wasted on a minor spell.
    fn counterable_spell_exists(&self, text: &str, player_id: &str) -> bool {
        let Some(view) = &self.view else { return true };
        view.stack.iter().any(|item| {
            if item.controller_id == player_id || item.is_casting || !item.is_spell
                || self.answers_another_opponent(item, player_id) {
                return false;
            }
            let types: Vec<String> = item.source_card.as_ref().map(|card| card.types.clone()).unwrap_or_default();
            if !Self::counter_clause_allows(text, &types, true) {
                return false;
            }
            let target = TargetRef { kind: TargetKind::Spell, id: item.id.clone(), intent: None, oracle: None };
            threat::spell_importance(view, item, player_id) >= threat::COUNTER_MIN
                && !self.covered_by_friendly_answer(&target, player_id)
        })
    }

    /// Whether "counter target <qualifier> spell" can take a spell with these card types.
    /// An unrecognized qualifier, or text with no counter clause, gives `unknown`: the target
    /// prompt decides a cast, while a counter already on the stack only counts when it is known.
    fn counter_clause_allows(text: &str, types: &[String], unknown: bool) -> bool {
        let Some(after) = text.split("counter target ").nth(1) else { return unknown };
        let qualifier = after.split("spell").next().unwrap_or("").trim();
        let has = |ty: &str| types.iter().any(|t| t.eq_ignore_ascii_case(ty));
        if qualifier.is_empty() {
            return true;
        }
        if let Some(excluded) = qualifier.strip_prefix("non") {
            let excluded = excluded.trim_start_matches('-');
            return !(KNOWN_TYPES.contains(&excluded) && has(excluded));
        }
        let wanted: Vec<&str> = qualifier
            .split([',', ' '])
            .map(str::trim)
            .filter(|word| !word.is_empty() && *word != "or" && *word != "and")
            .collect();
        if wanted.iter().all(|word| KNOWN_TYPES.contains(word)) {
            return wanted.iter().any(|word| has(word));
        }
        unknown
    }

    /// Whether `source` (a spell or ability of `player_id`) may target `card` at all: not under
    /// Shroud, not an opponent's Hexproof, not Protection from a colour the source has. The engine
    /// never offers such a permanent as a candidate, so a cast must not count it either.
    fn can_target(&self, card: &CardDto, source: Option<&CardDto>, player_id: &str) -> bool {
        const COLOR_NAMES: [(char, &str); 5] = [('W', "white"), ('U', "blue"), ('B', "black"), ('R', "red"), ('G', "green")];
        card.keywords.iter().map(|keyword| keyword.to_ascii_lowercase()).all(|keyword| {
            if keyword.contains("shroud") {
                return false;
            }
            if keyword.contains("hexproof") && card.controller_id != player_id {
                return false;
            }
            if keyword.contains("protection") {
                let from_all = keyword.contains("everything") || keyword.contains("all colors");
                let from_source = source.is_some_and(|source| COLOR_NAMES.iter().any(|(letter, name)| source.color.contains(*letter) && keyword.contains(name)));
                return !(from_all || from_source);
            }
            true
        })
    }

    /// What exiling and returning `card` gains, for a blink spell: none when it gains nothing,
    /// our own stack already removes it, or `source` cannot target it. The one rule for both the
    /// cast check and the target prompt, so they cannot disagree.
    fn reuse_value(&self, card: &CardDto, player_id: &str, source: Option<&CardDto>) -> Option<i32> {
        self.reentry_value(card, player_id)
            .filter(|&value| value > 0 && !self.removed_by_stack(player_id, &card.id) && self.can_target(card, source, player_id))
    }

    /// A permanent of ours that exiling and returning improves, for a blink spell.
    fn reuse_target_exists(&self, player_id: &str, creatures_only: bool, source: Option<&CardDto>) -> bool {
        self.battlefield(player_id)
            .filter(|card| !creatures_only || card.types.iter().any(|ty| ty == "Creature"))
            .any(|card| self.reuse_value(card, player_id, source).is_some())
    }

    /// The kind of direct answer a spell's text is: "Destroy target ...", "Exile target ...",
    /// "Return target ... to its owner's hand". `None` for anything else, including a blink.
    fn direct_answer_intent(effect: &str) -> Option<TargetingIntent> {
        let effect = effect.trim_start();
        if effect.starts_with("exile ") && effect.contains("return") && effect.contains("to the battlefield") {
            return None;
        }
        if !effect.contains("target ") {
            return None;
        }
        if effect.starts_with("destroy ") {
            Some(TargetingIntent::Destroy)
        } else if effect.starts_with("exile ") {
            Some(TargetingIntent::Exile)
        } else if effect.starts_with("return ") && effect.contains("to its owner's hand") {
            Some(TargetingIntent::Bounce)
        } else {
            None
        }
    }

    /// Whether `target` is worth a voluntary direct answer from `player_id`: targetable by the
    /// source, not a spell or permanent our side already answers, and not our own permanent
    /// unless a bounce saves it. The one rule for the cast check (over every permanent that could
    /// be the target) and the target prompt (over the engine's candidates).
    fn answer_target_worthwhile(&self, target: &TargetRef, intent: TargetingIntent, source: Option<&CardDto>, player_id: &str) -> bool {
        match target.kind {
            TargetKind::Spell => {
                let item = self.view.as_ref().and_then(|view| view.stack.iter().find(|item| item.id == target.id));
                let own = matches!(intent, TargetingIntent::Counter) && item.is_some_and(|item| item.controller_id == player_id);
                let third_party = matches!(intent, TargetingIntent::Counter) && item.is_some_and(|item| self.answers_another_opponent(item, player_id));
                !own && !third_party && !self.covered_by_friendly_answer(target, player_id)
            }
            TargetKind::Card => {
                let Some(card) = self.card(&target.id) else { return true };
                if !self.can_target(card, source, player_id) {
                    return false;
                }
                // Our own permanent is no answer target unless the text asks for one of ours or a
                // bounce saves it; cards in a graveyard or other zone are not permanents to answer.
                let ours_by_text = source.is_some_and(|source| {
                    let text = source.text.to_ascii_lowercase();
                    text.contains("you control") || text.contains("your graveyard") || text.contains("from your")
                });
                if card.controller_id == player_id && card.zone == Some(ZoneKind::Battlefield) && !ours_by_text {
                    let saved = matches!(intent, TargetingIntent::Bounce)
                        && self.view.as_ref().is_some_and(|view| threat::threatened(view, card, player_id));
                    if !saved {
                        return false;
                    }
                }
                !self.covered_by_friendly_answer(target, player_id)
            }
            TargetKind::Player => true,
        }
    }

    /// Whether a direct answer spell has any permanent worth answering, by the same rule its
    /// target prompt applies. `true` when the text names a target this does not read.
    fn answer_target_exists(&self, effect: &str, intent: TargetingIntent, source: &CardDto, player_id: &str) -> bool {
        let Some(after) = effect.split("target ").nth(1) else { return true };
        let noun: String = after.split(['.', ',']).next().unwrap_or("")
            .split(" to its").next().unwrap_or("").split(" you control").next().unwrap_or("").trim().to_string();
        let nonland = noun.starts_with("nonland");
        let wanted = if noun.ends_with("creature") {
            "Creature"
        } else if noun.ends_with("artifact") {
            "Artifact"
        } else if noun.ends_with("enchantment") {
            "Enchantment"
        } else if noun.ends_with("permanent") {
            ""
        } else {
            return true;
        };
        let Some(view) = &self.view else { return true };
        view.zones.iter().filter(|zone| zone.zone == ZoneKind::Battlefield).flat_map(|zone| &zone.cards).any(|card| {
            let CardView::Visible(card) = card else { return false };
            let right_type = (wanted.is_empty() || card.types.iter().any(|ty| ty == wanted))
                && !(nonland && card.types.iter().any(|ty| ty == "Land"));
            let target = TargetRef { kind: TargetKind::Card, id: card.id.clone(), intent: None, oracle: None };
            right_type && self.answer_target_worthwhile(&target, intent, Some(source), player_id)
        })
    }

    fn sane(&self, action: &AvailableAction, player_id: &str) -> bool {
        let AvailableActionKind::Cast { card_id, mode, .. } = &action.kind else {
            return true;
        };
        if self.is_land_play(action) {
            return true;
        }
        let Some(card) = self.card(card_id) else {
            return true;
        };
        let offered_blitz = matches!(mode, PlayCardMode::Alternative { cost: AlternativeCostKind::Blitz });
        if !offered_blitz && !self.affordable(card, player_id) {
            // The engine's offered action already checks static reductions.
            // Our estimate uses printed mana value and otherwise discards
            // payable discounted spells before the payment session.
            let has_reducer = self.battlefield(player_id).any(|source| {
                let text = source.text.to_ascii_lowercase();
                text.contains("cost") && text.contains("less to cast")
            });
            if !has_reducer { return false; }
        }
        let Some(view) = self.view.as_ref() else {
            return true;
        };
        let text = card.text.to_ascii_lowercase();
        let own_turn = view.active_player_id == player_id;
        let foreign_stack = view
            .stack
            .iter()
            .any(|item| item.controller_id != player_id);
        let opponents = self.opponent_creatures(player_id);
        // Mass phasing of our own board is a defensive response, not ordinary
        // main-phase development when no opposing spell is on the stack.
        if text.contains("permanents you control phase out") {
            let threatened = view.stack.iter().filter(|item| item.controller_id != player_id).any(|item| {
                let effect = item.source_ability_text.as_deref().filter(|s| !s.is_empty()).unwrap_or(&item.text).to_ascii_lowercase();
                let destructive = ["destroy", "exile", "sacrifice", "damage", "return", "-x/-x", "-1/-1"].iter().any(|word| effect.contains(word));
                let wide = ["all creature", "each creature", "all permanent", "each permanent", "each nonland"].iter().any(|word| effect.contains(word));
                let ours = item.targets.iter().any(|target| self.card(&target.id).is_some_and(|c| c.controller_id == player_id));
                destructive && (wide || ours)
            });
            if !threatened { return false; }
        }
        let roles = Self::roles(card);
        if own_turn && view.step == StepKind::Main1 && !foreign_stack
            && card.types.iter().any(|t| t == "Instant")
            && (roles.contains(Roles::DRAW) || text.contains("draw"))
            && self.battlefield(player_id).any(|c| {
                let ability = c.text.to_ascii_lowercase();
                c.types.iter().any(|t| t == "Creature") && !c.tapped && !c.summoning_sick
                    && ability.contains("whenever you cast a spell while") && ability.contains("is attacking") && ability.contains("copy")
            }) {
            // Wait for an available attack-copy window; if no attack is made,
            // normal main-two selection remains available.
            return false;
        }
        // "Exile target creature you control, then return it" is a blink, not removal.
        let own_target = text.contains("target creature you control");
        let creature_removal = (text.contains("destroy target creature")
            || text.contains("exile target creature")
            || (text.contains("damage to target creature") && !text.contains("player")))
            && !own_target;
        if creature_removal && opponents.is_empty() {
            return false;
        }
        if roles.contains(Roles::REMOVAL)
            && !roles.contains(Roles::BURN)
            && self.opponent_permanents(player_id) == 0
        {
            return false;
        }
        if (roles.contains(Roles::COUNTERSPELL) || text.starts_with("counter target"))
            && !foreign_stack
        {
            return false;
        }
        // A counterspell that names a spell needs an opposing spell it can take: an ability
        // on the stack, our own spell, or a spell already answered would only be started and
        // cancelled. Free and alternative-cost counters (Force of Will) put the clause after
        // their cost text, so look for it anywhere.
        if text.contains("counter target")
            && text.contains("spell")
            && !text.contains("ability")
            && !self.counterable_spell_exists(&text, player_id)
        {
            return false;
        }
        // A protection spell is cast for a valuable permanent an opposing spell or ability is aimed
        // at, and held otherwise.
        if threat::is_protection(&text)
            && !card.types.iter().any(|ty| ty == "Creature")
            && threat::most_threatened_importance(view, player_id).is_none_or(|value| value < threat::PROTECT_MIN)
        {
            return false;
        }
        // A blink spell is worth casting only for a permanent that gains from re-entering
        // (an entry ability, or a threat to save it). Otherwise its target prompt would be
        // cancelled.
        if card.types.iter().any(|ty| ty == "Instant" || ty == "Sorcery")
            && text.contains("exile")
            && text.contains("target creature")
            && text.contains("you control")
            && text.contains("return")
            && text.contains("to the battlefield")
            && !self.reuse_target_exists(player_id, true, Some(card))
        {
            return false;
        }
        // A direct answer is cast only when the target prompt would find a target worth it.
        if let Some(intent) = Self::direct_answer_intent(&text) {
            if !self.answer_target_exists(&text, intent, card, player_id) {
                return false;
            }
        }
        // The same cast was cancelled at target selection against this very board.
        if self.cancelled_plays.get(card_id.as_str()) == Some(&self.board_signature()) {
            return false;
        }
        if roles.contains(Roles::COMBAT_PUMP)
            && !foreign_stack
            && !card.types.iter().any(|ty| ty == "Creature")
            && !matches!(
                view.step,
                StepKind::CombatDeclareAttackers
                    | StepKind::CombatDeclareBlockers
                    | StepKind::CombatFirstStrikeDamage
            )
        {
            return false;
        }
        let wipe = roles.contains(Roles::SWEEPER)
            || text.contains("destroy all creatures")
            || text.contains("exile all creatures")
            || text.contains("each creature")
                && (text.contains("destroy") || text.contains("-x/-x"));
        if wipe {
            let mine: i32 = self
                .battlefield(player_id)
                .filter(|c| c.types.iter().any(|ty| ty == "Creature"))
                .map(Self::card_value)
                .sum();
            let theirs: i32 = opponents.iter().map(|c| Self::card_value(c)).sum();
            if mine >= theirs {
                return false;
            }
        }
        let instant = card.types.iter().any(|ty| ty == "Instant")
            || (card
                .keywords
                .iter()
                .any(|k| k.eq_ignore_ascii_case("flash"))
                && !card.types.iter().any(|ty| ty == "Creature"));
        // The player's own triggers on the stack are not a reason to respond.
        if instant && !foreign_stack {
            let step = view.step;
            let quiet_own = own_turn && matches!(step, StepKind::Upkeep | StepKind::Draw);
            let quiet_theirs = !own_turn
                && matches!(
                    step,
                    StepKind::Untap
                        | StepKind::Upkeep
                        | StepKind::Draw
                        | StepKind::Main1
                        | StepKind::Main2
                        | StepKind::CombatBegin
                );
            if quiet_own || quiet_theirs {
                return false;
            }
        }
        true
    }

    fn land_choice_score(&self, card: &CardDto, player_id: &str, alternatives: &[&CardDto]) -> i32 {
        let mut supply = [0i32; 5];
        let lands = self.lands_in_play(player_id) as i32;
        for land in self
            .battlefield(player_id)
            .filter(|c| c.types.iter().any(|ty| ty == "Land"))
        {
            for color in Self::land_colors(land) {
                if let Some(i) = "WUBRG".find(color) {
                    supply[i] += 1;
                }
            }
        }
        let mut demand = [0i32; 5];
        for hand_card in self
            .view
            .iter()
            .flat_map(|view| &view.zones)
            .filter(|zone| {
                matches!(zone.zone, ZoneKind::Hand | ZoneKind::Command)
                    && zone.owner_id == player_id
            })
            .flat_map(|zone| &zone.cards)
            .filter_map(|card| match card {
                CardView::Visible(card) => Some(card),
                CardView::Hidden { .. } => None,
            })
            .filter(|c| !c.types.iter().any(|ty| ty == "Land") && c.cmc <= lands + 1)
        {
            let mut pips = [0i32; 5];
            for symbol in hand_card.mana_cost.chars() {
                if let Some(i) = "WUBRG".find(symbol) {
                    pips[i] += 1;
                }
            }
            for i in 0..5 {
                demand[i] = demand[i].max(pips[i]);
            }
        }
        let fixes = Self::land_colors(card)
            .into_iter()
            .filter_map(|color| "WUBRG".find(color))
            .filter(|&i| demand[i] > supply[i])
            .count()
            .min(2) as i32;
        let enters_tapped = card.text.contains("enters tapped");
        let untapped_alternative = alternatives
            .iter()
            .any(|other| other.id != card.id && !other.text.contains("enters tapped"));
        fixes * 20 - i32::from(enters_tapped && untapped_alternative) * 5
    }

    fn action_score(&self, action: &AvailableAction, player_id: &str) -> i32 {
        match &action.kind {
            AvailableActionKind::Cast { card_id, label, .. } => {
                let Some(card) = self.card(card_id) else {
                    return if label.starts_with("Play ") { 900 } else { 300 };
                };
                if label.starts_with("Play ")
                    || card.types.iter().any(|card_type| card_type == "Land")
                {
                    return 1_000 + self.land_choice_score(card, player_id, &[]);
                }
                let own_turn = self
                    .view
                    .as_ref()
                    .is_some_and(|view| view.active_player_id == player_id);
                let main_phase = self
                    .view
                    .as_ref()
                    .is_some_and(|view| matches!(view.step, StepKind::Main1 | StepKind::Main2));
                let permanent = card.types.iter().any(|card_type| {
                    matches!(
                        card_type.as_str(),
                        "Creature" | "Artifact" | "Enchantment" | "Planeswalker" | "Battle"
                    )
                });
                let instant = card.types.iter().any(|card_type| card_type == "Instant");
                let mut score = if own_turn && main_phase {
                    if permanent {
                        500
                    } else {
                        420
                    }
                } else if instant {
                    430
                } else {
                    250
                };
                score += Self::card_value(card) - card.cmc * 8;
                if self.mass_life_loss_wins(card, player_id) {
                    score += 5_000;
                }
                if self.card_zone(card_id) == Some(ZoneKind::Command) {
                    score += 160;
                }
                if self.prefers_creatures(player_id)
                    && card.types.iter().any(|card_type| card_type == "Creature")
                {
                    score += (120 - card.cmc * 12).max(0);
                }
                let text = card.text.to_ascii_lowercase();
                let roles = Self::roles(card);
                let ramp = Self::is_mana_source(card)
                    || roles.intersects(Roles::RAMP | Roles::MANA_DORK | Roles::LAND_FETCH)
                    || (text.contains("search your library for") && text.contains("land card"));
                if ramp && self.lands_in_play(player_id) < 6 {
                    score += 60;
                }
                if roles.contains(Roles::DRAW)
                    || text.contains("draw a card")
                    || text.contains("draw two")
                {
                    score += 35;
                }
                if roles.contains(Roles::REMOVAL) {
                    score += 30;
                }
                if roles.contains(Roles::TOKENS) {
                    score += 20;
                }
                if text.contains("whenever you cast")
                    && text.contains("creature spell")
                    && text.contains("draw")
                {
                    score += 180;
                }
                if text.contains("destroy target") || text.contains("exile target") {
                    score += 30;
                }
                if text.contains("create a") || text.contains("search your library") {
                    score += 20;
                }
                score
            }
            AvailableActionKind::ActivateAbility(info) if !info.is_mana_ability => {
                let text = info.description.to_ascii_lowercase();
                let land_search = text.starts_with("search your library for") && text.contains("land")
                    || ["plains", "island", "swamp", "mountain", "forest"].iter().any(|t| text.starts_with("search your library for a") && text.contains(t));
                if self.useful_lifelink_activation(info) || self.useful_creature_tutor_sacrifice(info) { 1_000 }
                // A land fetched before the spells are chosen adds its mana to this turn.
                else if land_search && info.cost.as_deref().is_some_and(|c| c.contains("CARDNAME")) { 700 }
                else if text.contains("search your library") {
                    240
                } else if text.contains("draw") || text.contains("create") {
                    210
                } else if text.contains("destroy") || text.contains("exile") {
                    200
                } else if text.contains("sacrifice") || text.contains("pay ") {
                    80
                } else {
                    120
                }
            }
            _ => 0,
        }
    }

    fn life_counter_pinger<'a>(&'a self, player_id: &'a str, require_lifelink: bool) -> Option<&'a CardDto> {
        let has_counter_engine = self.battlefield(player_id).any(|card| {
            let text = card.text.to_ascii_lowercase();
            text.contains("whenever you gain life") && text.contains("+1/+1 counter")
        });
        if !has_counter_engine { return None; }
        self.battlefield(player_id).find(|card| {
            let text = card.text.to_ascii_lowercase();
            text.contains("remove a +1/+1 counter") && text.contains("damage to any target")
                && card.counters.iter().any(|(name,count)| (name.contains("+1/+1") || name.eq_ignore_ascii_case("P1P1")) && *count >= 1)
                && (!require_lifelink || Self::has_keyword(card, "Lifelink"))
        })
    }

    fn death_tutor_plan_target<'a>(&'a self, player: &'a str) -> Option<&'a CardDto> {
        let view=self.view.as_ref()?;
        if !view.stack.is_empty() { return None; }
        let life=view.players.iter().find(|p|p.id==player)?.life;
        let library=view.zones.iter().find(|z|z.owner_id==player && z.zone==ZoneKind::Library).map(|z|z.count).unwrap_or(0);
        if life<=7 || library==0 || self.known_tutor_targets.as_ref().is_some_and(|known|known.is_empty()) { return None; }
        let is_tutor = |card: &CardDto| {
            let text=card.text.to_ascii_lowercase();
            text.contains("dies") && text.contains("search your library") && text.contains("creature cards") && text.contains("onto the battlefield")
        };
        if let Some(tutor)=self.battlefield(player).find(|card|is_tutor(card)) { return Some(tutor); }
        let tutor_in_grave=view.zones.iter().any(|zone|zone.owner_id==player && zone.zone==ZoneKind::Graveyard
            && zone.cards.iter().any(|card|matches!(card,CardView::Visible(card) if is_tutor(card))));
        let undying_engine=self.battlefield(player).any(|card| {
            let text=card.text.to_ascii_lowercase();text.contains("other non-human creatures you control") && text.contains("have undying")
        });
        if tutor_in_grave && undying_engine {
            return self.battlefield(player).find(|card| {
                card.text.to_ascii_lowercase().contains("return target creature card from your graveyard to the battlefield")
                    && !card.counters.iter().any(|(name,count)|(name.contains("+1/+1") || name.eq_ignore_ascii_case("P1P1")) && *count>0)
            });
        }
        None
    }

    fn useful_creature_tutor_sacrifice(&self, info: &ActivatableAbilityInfo) -> bool {
        let Some(source)=self.card(&info.card_id) else { return false; };
        info.cost.as_deref().is_some_and(|cost|cost.to_ascii_lowercase().contains("sac"))
            && self.death_tutor_plan_target(&source.controller_id).is_some()
    }

    fn useful_lifelink_activation(&self, info: &ActivatableAbilityInfo) -> bool {
        let Some(source) = self.card(&info.card_id) else { return false; };
        info.description.to_ascii_lowercase().contains("target creature gains lifelink")
            && self.life_counter_pinger(&source.controller_id, false)
                .is_some_and(|card| !Self::has_keyword(card, "Lifelink") && card.counters.values().any(|count| *count >= 2))
    }

    fn zone_cards(&self, player_id: &str, kind: ZoneKind) -> Vec<&CardDto> {
        self.view.iter().flat_map(|view| &view.zones)
            .filter(|zone| zone.zone == kind && zone.owner_id == player_id)
            .flat_map(|zone| &zone.cards)
            .filter_map(|card| match card { CardView::Visible(card) => Some(card), CardView::Hidden { .. } => None })
            .collect()
    }

    fn library_count(&self, player_id: &str) -> usize {
        self.view.iter().flat_map(|view| &view.zones)
            .find(|zone| zone.zone == ZoneKind::Library && zone.owner_id == player_id)
            .map_or(0, |zone| zone.count)
    }

    fn life_of(&self, player_id: &str) -> i32 {
        self.view.as_ref().and_then(|view| view.players.iter().find(|p| p.id == player_id)).map_or(0, |p| p.life)
    }

    /// Life paid for an effect: "pay 2 life", "Pay 1 {LIFE}".
    fn life_paid(text: &str) -> i32 {
        let text = text.to_ascii_lowercase();
        text.split("pay ").skip(1).filter_map(|part| {
            let mut words = part.split_whitespace();
            let amount = words.next()?.parse::<i32>().ok()?;
            words.next().filter(|word| word.trim_matches(|c: char| !c.is_alphabetic()) == "life")?;
            Some(amount)
        }).max().unwrap_or(0)
    }

    /// Life that must remain after paying `cost`: the bigger the cost, the more cushion.
    fn can_spare_life(&self, player_id: &str, cost: i32) -> bool {
        cost <= 0 || self.life_of(player_id) >= 10 + 2 * cost
    }

    /// A creature the bot would give up for value: a token or a small, non-legendary body.
    fn is_fodder(card: &CardDto) -> bool {
        card.types.iter().any(|t| t == "Creature") && card.commander_tax.is_none()
            && !card.supertypes.iter().any(|t| t == "Legendary")
            && (card.identity.is_token || Self::card_value(card) <= 12)
    }

    /// Whether a sacrifice ability gains more than it gives up. A spent permanent (fetch land,
    /// Clue, Mind Stone, Sakura-Tribe Elder) is cheap to give up; another permanent must be fodder.
    fn worthwhile_sacrifice(&self, info: &ActivatableAbilityInfo) -> bool {
        let (Some(source), Some(view)) = (self.card(&info.card_id), self.view.as_ref()) else { return false; };
        let me = source.controller_id.as_str();
        let cost = info.cost.as_deref().unwrap_or("").to_ascii_lowercase();
        let text = info.description.to_ascii_lowercase();
        let own_turn = view.active_player_id == me;
        let quiet = view.stack.is_empty();
        let main = own_turn && quiet && matches!(view.step, StepKind::Main1 | StepKind::Main2);
        let idle = quiet && ((own_turn && view.step == StepKind::Main2) || view.step == StepKind::EndOfTurn);
        if !self.can_spare_life(me, Self::life_paid(&cost)) { return false; }
        let self_sacrifice = cost.contains("cardname");
        let searches = text.starts_with("search your library for") && self.library_count(me) > 0;
        if self_sacrifice {
            if searches && (text.contains("land") || ["plains", "island", "swamp", "mountain", "forest"].iter().any(|t| text.contains(t))) {
                return main;
            }
            // Cards, not a rock: a mana rock is sacrificed once the lands carry the mana.
            let rock = Self::is_mana_source(source);
            if text.starts_with("draw a card") || text.contains("explores") {
                return idle && (!rock || self.lands_in_play(me) >= 6) && self.library_count(me) >= 6;
            }
            if text.contains("gain haste") {
                let slow: i32 = self.battlefield(me).filter(|c| c.summoning_sick && !c.tapped)
                    .filter_map(|c| c.power.as_deref().and_then(|p| p.parse::<i32>().ok())).sum();
                return own_turn && quiet && view.step == StepKind::Main1 && slow >= 4;
            }
            return false;
        }
        // Another permanent: spend a creature that is fodder, for a tutor.
        if searches && cost.contains("creature") && !cost.contains("amount: x") {
            return main && self.battlefield(me).any(|c| c.id != source.id && Self::is_fodder(c));
        }
        false
    }

    /// Whether an optional ("you may") effect that benefits only the deciding player is worth
    /// taking. Gains: search, draw, create, copy. Costs (life, discard) must be worth it.
    fn accepts_optional_effect(&self, title: &str, source_id: &str, player_id: &str) -> bool {
        let Some(view) = self.view.as_ref() else { return false; };
        let paying = title.contains("sacrifice") || title.contains("pay {") || title.contains("discard your hand")
            || title.contains("lose ");
        if paying { return false; }
        let library = self.library_count(player_id);
        let lands = self.lands_in_play(player_id);
        let life_cost = Self::life_paid(title);
        if !self.can_spare_life(player_id, life_cost) { return false; }
        // Free cast of a spell (Swan Song, Rite of Replication): only for a spell with a use.
        if title.starts_with("do you want to play ") {
            let Some(card) = self.card(source_id) else { return true; };
            let text = card.text.to_ascii_lowercase();
            if Self::roles(card).contains(Roles::COUNTERSPELL) || text.starts_with("counter target") {
                return view.stack.iter().any(|item| item.controller_id != player_id);
            }
            if text.contains("destroy target creature") || text.contains("exile target creature") {
                return !self.opponent_creatures(player_id).is_empty();
            }
            return true;
        }
        let searches = title.contains("search your library") || (title.starts_with("search ") && title.ends_with("'s library?"));
        if searches { return library > 0; }
        let draws = title.contains("draw ") || title.starts_with("do you want to draw ");
        if title.contains("you may discard") && draws {
            // Discard only what is worth less than the card: a land beyond need, or a dead card.
            let hand = self.zone_cards(player_id, ZoneKind::Hand);
            let spare = hand.iter().any(|card| {
                let land = card.types.iter().any(|t| t == "Land");
                (land && lands >= 5) || (!land && card.cmc >= lands as i32 + 4)
            });
            return spare && library >= 6;
        }
        if draws && !title.contains("discard") {
            // Keep the deck-out guard: never draw down to the last few cards.
            return library >= 6;
        }
        if title.contains("become a copy") {
            return view.zones.iter().filter(|z| z.zone == ZoneKind::Battlefield).flat_map(|z| &z.cards).any(|card| matches!(card,
                CardView::Visible(card) if card.types.iter().any(|t| t == "Creature") && card.id != source_id
                    && card.power.as_deref().and_then(|p| p.parse::<i32>().ok()).unwrap_or(0) >= 2));
        }
        if title.contains("you may put a +1/+1 counter on") { return true; }
        if title.contains("create a token that's a copy") || (title.contains("you may create") && !title.contains("exile")) {
            return true;
        }
        // A shock land entering: untapped when this turn needs the mana or life is comfortable.
        if title.starts_with("pay ") && title.contains("{life} for ") && !self.card(source_id).is_some_and(|c| c.types.iter().any(|t| t == "Land")) {
            // Paying life for the source's own "you may pay N life. If you do, ..." gain.
            let Some(text) = self.card(source_id).map(|c| c.text.to_ascii_lowercase()) else { return false; };
            let Some(reward) = text.split("you may pay ").nth(1).and_then(|rest| rest.split("if you do, ").nth(1)) else { return false; };
            return (reward.starts_with("draw") && library >= 6) || (reward.starts_with("search") && library > 0) || reward.starts_with("create");
        }
        if title.starts_with("pay 2 {life} for ") {
            let Some(land) = self.card(source_id).filter(|card| card.types.iter().any(|t| t == "Land")) else { return false; };
            let untapped = self.battlefield(player_id).filter(|c| c.id != land.id && !c.tapped && Self::is_mana_source(c)).count() as i32;
            let needed = view.active_player_id == player_id && self.zone_cards(player_id, ZoneKind::Hand).iter()
                .any(|card| !card.types.iter().any(|t| t == "Land") && card.cmc == untapped + 1);
            return needed || self.life_of(player_id) >= 20;
        }
        false
    }

    /// The caster's pile: the one with more value, by the revealed cards' values. Cards are
    /// listed pile 1 first, then pile 2; `split` is how many belong to pile 1.
    fn better_pile_card(cards: &[CardDto], split: usize) -> Option<String> {
        let value = |card: &CardDto| if card.types.iter().any(|t| t == "Land") { 4 } else { Self::card_value(card).max(5) };
        let (first, second) = cards.split_at(split.min(cards.len()));
        let pile_value = |pile: &[CardDto]| pile.iter().map(value).sum::<i32>();
        let pile = if pile_value(first) >= pile_value(second) { first } else { second };
        pile.iter().max_by_key(|card| value(card)).map(|card| card.id.clone())
    }

    /// The separator's piles: the split that leaves the caster the least, i.e. the lowest
    /// value in the bigger-valued pile, found over every division of the cards.
    fn pile_one_cards(cards: &[CardDto], min: usize, max: usize) -> Vec<String> {
        let value = |card: &CardDto| if card.types.iter().any(|t| t == "Land") { 4 } else { Self::card_value(card).max(5) };
        let values: Vec<i32> = cards.iter().map(value).collect();
        let total: i32 = values.iter().sum();
        let n = cards.len().min(16);
        let mut best: Option<(i32, u32)> = None;
        for mask in 1u32..(1 << n) {
            let size = mask.count_ones() as usize;
            if size < min || size > max { continue; }
            let one: i32 = (0..n).filter(|i| mask & (1 << i) != 0).map(|i| values[i]).sum();
            let worst = one.max(total - one);
            if best.is_none_or(|(w, _)| worst < w) { best = Some((worst, mask)); }
        }
        best.map(|(_, mask)| (0..n).filter(|i| mask & (1 << i) != 0).map(|i| cards[i].id.clone()).collect()).unwrap_or_default()
    }

    fn wasted_activation(&self, info: &ActivatableAbilityInfo) -> bool {
        let text = info.description.to_ascii_lowercase();
        if let Some(source) = self.card(&info.card_id) {
            let pending_lifelink = self.view.as_ref().is_some_and(|view| view.stack.iter().any(|item| {
                item.controller_id == source.controller_id && item.source_ability_text.as_deref().unwrap_or(&item.text).to_ascii_lowercase().contains("lifelink")
            }));
            if pending_lifelink && self.life_counter_pinger(&source.controller_id, false).is_some_and(|card| card.id == source.id && !Self::has_keyword(card, "Lifelink")) { return true; }
        }
        if self.useful_lifelink_activation(info) || self.useful_creature_tutor_sacrifice(info) { return false; }
        let sacrifices = info.cost.as_deref().is_some_and(|cost| cost.to_ascii_lowercase().contains("sac"));
        if (sacrifices && !self.worthwhile_sacrifice(info))
            || text.contains("any player may activate")
            || (text.contains(" loses ") && !text.contains("life"))
        {
            return true;
        }
        let main_phase = self
            .view
            .as_ref()
            .is_some_and(|view| matches!(view.step, StepKind::Main1 | StepKind::Main2));
        main_phase
            && text.contains("until end of turn")
            && (text.contains("gets +") || text.contains("get +") || text.contains("gains "))
    }

    fn has_keyword(card: &CardDto, keyword: &str) -> bool {
        card.keywords
            .iter()
            .any(|value| value.eq_ignore_ascii_case(keyword))
    }

    fn is_constructed_duel(&self) -> bool {
        !self.has_command_cards
            && self
                .view
                .as_ref()
                .is_some_and(|view| view.players.len() == 2)
    }

    fn blockers_to_keep(&self, player_id: &str, attackers: &[String]) -> HashSet<String> {
        let mut keep = HashSet::new();
        let Some(view) = &self.view else {
            return keep;
        };
        let life = view
            .players
            .iter()
            .find(|player| player.id == player_id)
            .map_or(20, |player| player.life);
        let mut threats = view
            .players
            .iter()
            .filter(|player| player.id != player_id)
            .map(|player| {
                let mut powers = view
                    .zones
                    .iter()
                    .filter(|zone| zone.zone == ZoneKind::Battlefield && zone.owner_id == player.id)
                    .flat_map(|zone| &zone.cards)
                    .filter_map(|card| match card {
                        CardView::Visible(card) if card.types.iter().any(|ty| ty == "Creature") => {
                            Some(Self::stat(card.power.as_deref()).max(0))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                powers.sort_unstable_by(|left, right| right.cmp(left));
                powers
            })
            .max_by_key(|powers| powers.iter().sum::<i32>())
            .unwrap_or_default();
        let mut incoming = threats.iter().sum::<i32>();
        if !Self::life_in_danger(life, incoming, false) {
            return keep;
        }
        let mut candidates = attackers
            .iter()
            .filter_map(|id| self.combatant(id))
            .filter(|creature| {
                !self
                    .card(&creature.id)
                    .is_some_and(|card| Self::has_keyword(card, "Vigilance"))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|creature| std::cmp::Reverse((creature.lethal, creature.power)));
        for creature in candidates {
            if !Self::life_in_danger(life, incoming, false) {
                break;
            }
            if threats.is_empty() {
                break;
            }
            incoming -= threats.remove(0);
            keep.insert(creature.id);
        }
        // Blockers that still leave us in danger save nothing; they attack instead.
        if Self::life_in_danger(life, incoming, false) {
            keep.clear();
        }
        keep
    }

    fn battlefield<'a>(&'a self, player_id: &'a str) -> impl Iterator<Item = &'a CardDto> + 'a {
        self.view
            .iter()
            .flat_map(|view| &view.zones)
            .filter(move |zone| zone.zone == ZoneKind::Battlefield && zone.owner_id == player_id)
            .flat_map(|zone| &zone.cards)
            .filter_map(|card| match card {
                CardView::Visible(card) => Some(card),
                CardView::Hidden { .. } => None,
            })
    }

    fn is_mana_source(card: &CardDto) -> bool {
        card.types.iter().any(|ty| ty == "Land")
            || (!card.types.iter().any(|ty| ty == "Creature")
                && card.text.to_ascii_lowercase().contains("{t}: add"))
    }

    fn available_mana(&self, player_id: &str) -> i32 {
        self.battlefield(player_id)
            .filter(|card| !card.tapped && Self::is_mana_source(card))
            .count() as i32
    }

    fn lands_in_play(&self, player_id: &str) -> usize {
        self.battlefield(player_id)
            .filter(|card| card.types.iter().any(|ty| ty == "Land"))
            .count()
    }

    fn should_attack(&self, attacker_id: &str, target_id: &str) -> bool {
        let (Some(attacker), Some(me)) = (self.card(attacker_id), self.combatant(attacker_id))
        else {
            return true;
        };
        if me.power <= 0 {
            return false;
        }
        let Some(view) = &self.view else {
            return true;
        };
        let flying = Self::has_keyword(attacker, "Flying");
        let menace = Self::has_keyword(attacker, "Menace");
        let blockers: Vec<Combatant> = view
            .zones
            .iter()
            .filter(|zone| zone.zone == ZoneKind::Battlefield && zone.owner_id == target_id)
            .flat_map(|zone| &zone.cards)
            .filter_map(|card| match card {
                CardView::Visible(card)
                    if card.types.iter().any(|card_type| card_type == "Creature")
                        && !card.tapped
                        && (!flying
                            || Self::has_keyword(card, "Flying")
                            || Self::has_keyword(card, "Reach")) =>
                {
                    self.combatant(&card.id)
                }
                _ => None,
            })
            .collect();
        // An engine creature or commander does not trade into a cheaper body.
        let key = self.importance(attacker) >= threat::KEY_PERMANENT;
        let losing = |blocker: &Combatant| {
            Self::can_destroy(blocker, &me)
                && (!Self::can_destroy(&me, blocker) || (key && blocker.value < me.value))
        };
        if menace {
            let killers = blockers.iter().filter(|b| losing(b)).count();
            return killers < 2;
        }
        !blockers.iter().any(losing)
    }

    fn stat(value: Option<&str>) -> i32 {
        value
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0)
    }

    fn combatant(&self, id: &str) -> Option<Combatant> {
        let card = self.card(id)?;
        Some(Combatant {
            id: id.to_string(),
            power: Self::stat(card.power.as_deref()).max(0),
            lethal: (Self::stat(card.toughness.as_deref()) - card.damage).max(1),
            value: Self::card_value(card),
            deathtouch: Self::has_keyword(card, "Deathtouch"),
            first_strike: Self::has_keyword(card, "First strike")
                || Self::has_keyword(card, "Double strike"),
            double_strike: Self::has_keyword(card, "Double strike"),
            indestructible: Self::has_keyword(card, "Indestructible"),
            trample: Self::has_keyword(card, "Trample"),
        })
    }

    fn can_destroy(striker: &Combatant, target: &Combatant) -> bool {
        if target.indestructible || striker.power <= 0 {
            return false;
        }
        let damage = if striker.double_strike {
            striker.power * 2
        } else {
            striker.power
        };
        let kills = striker.deathtouch || damage >= target.lethal;
        let struck_first = target.first_strike
            && !striker.first_strike
            && (target.deathtouch || target.power >= striker.lethal);
        kills && !struck_first
    }

    fn life_in_danger(life: i32, unblocked: i32, serious: bool) -> bool {
        let threshold = if serious { 1 } else { 4.min(life) };
        life - unblocked < threshold
    }

    fn declare_blockers(
        &self,
        attackers: &[manabrew_protocol::prompts::choose_blockers::BlockableAttackerDto],
        available_blocker_ids: &[String],
        player_id: &str,
    ) -> Vec<BlockAssignment> {
        let life = self.view.as_ref().map_or(20, |view| {
            view.players
                .iter()
                .find(|player| player.id == player_id)
                .map_or(20, |player| player.life)
        });
        let mut attackers = attackers
            .iter()
            .filter_map(|attacker| {
                let combatant = self.combatant(&attacker.attacker_id)?;
                let at_me = self
                    .card(&attacker.attacker_id)
                    .is_some_and(|card| card.attacking_player_id.as_deref() == Some(player_id));
                Some((attacker, combatant, at_me))
            })
            .collect::<Vec<_>>();
        attackers.sort_by_key(|(_, combatant, _)| {
            std::cmp::Reverse((!combatant.trample, combatant.power, combatant.value))
        });
        let mut blockers = available_blocker_ids
            .iter()
            .filter_map(|id| self.combatant(id))
            .collect::<Vec<_>>();
        blockers.sort_by_key(|blocker| (blocker.power, blocker.value));

        let mut assignments: Vec<BlockAssignment> = Vec::new();
        let mut blocked: HashSet<String> = HashSet::new();
        let mut used: HashSet<String> = HashSet::new();
        let unblocked_damage = |blocked: &HashSet<String>| {
            attackers
                .iter()
                .filter(|(attacker, _, at_me)| *at_me && !blocked.contains(&attacker.attacker_id))
                .map(|(_, combatant, _)| combatant.power)
                .sum::<i32>()
        };
        let candidates =
            |attacker: &manabrew_protocol::prompts::choose_blockers::BlockableAttackerDto,
             used: &HashSet<String>| {
                blockers
                    .iter()
                    .filter(|blocker| {
                        !used.contains(&blocker.id)
                            && attacker.valid_blocker_ids.contains(&blocker.id)
                    })
                    .collect::<Vec<_>>()
            };
        let assign = |attacker_id: &str,
                      blocker: &Combatant,
                      assignments: &mut Vec<BlockAssignment>,
                      blocked: &mut HashSet<String>,
                      used: &mut HashSet<String>| {
            assignments.push(BlockAssignment {
                blocker_id: blocker.id.clone(),
                attacker_id: attacker_id.to_string(),
            });
            blocked.insert(attacker_id.to_string());
            used.insert(blocker.id.clone());
        };

        let good_blocks = |assignments: &mut Vec<BlockAssignment>,
                           blocked: &mut HashSet<String>,
                           used: &mut HashSet<String>| {
            for (attacker, combatant, _) in &attackers {
                if attacker.min_blockers > 1 || blocked.contains(&attacker.attacker_id) {
                    continue;
                }
                let options = candidates(attacker, used);
                let safe = options
                    .iter()
                    .filter(|blocker| !Self::can_destroy(combatant, blocker))
                    .copied()
                    .collect::<Vec<_>>();
                let pick = safe
                    .iter()
                    .find(|blocker| Self::can_destroy(blocker, combatant))
                    .or_else(|| safe.iter().find(|_| !combatant.trample))
                    .copied();
                if let Some(blocker) = pick {
                    assign(&attacker.attacker_id, blocker, assignments, blocked, used);
                }
            }
        };
        let gang_blocks = |assignments: &mut Vec<BlockAssignment>,
                           blocked: &mut HashSet<String>,
                           used: &mut HashSet<String>| {
            for (attacker, combatant, _) in &attackers {
                if blocked.contains(&attacker.attacker_id)
                    || attacker.max_blockers.is_some_and(|max| max < 2)
                {
                    continue;
                }
                let options = candidates(attacker, used);
                let pair = options.iter().enumerate().find_map(|(index, first)| {
                    options[index + 1..].iter().find_map(|second| {
                        let kills = first.power + second.power >= combatant.lethal
                            && !combatant.indestructible;
                        let loses = [first, second]
                            .into_iter()
                            .filter(|blocker| Self::can_destroy(combatant, blocker))
                            .map(|blocker| blocker.value)
                            .max()
                            .unwrap_or(0);
                        let both_die = combatant.power >= first.lethal + second.lethal
                            || (combatant.deathtouch && combatant.power >= 2);
                        (kills && !both_die && loses <= combatant.value).then_some((first, second))
                    })
                });
                if let Some((first, second)) = pair {
                    assign(&attacker.attacker_id, first, assignments, blocked, used);
                    assign(&attacker.attacker_id, second, assignments, blocked, used);
                }
            }
        };
        let trade_blocks = |danger: bool,
                            assignments: &mut Vec<BlockAssignment>,
                            blocked: &mut HashSet<String>,
                            used: &mut HashSet<String>| {
            for (attacker, combatant, _) in &attackers {
                if attacker.min_blockers > 1 || blocked.contains(&attacker.attacker_id) {
                    continue;
                }
                let pick = candidates(attacker, used).into_iter().find(|blocker| {
                    Self::can_destroy(blocker, combatant)
                        && (danger || blocker.value <= combatant.value)
                });
                if let Some(blocker) = pick {
                    assign(&attacker.attacker_id, blocker, assignments, blocked, used);
                }
            }
        };
        let chump_blocks = |serious: bool,
                            assignments: &mut Vec<BlockAssignment>,
                            blocked: &mut HashSet<String>,
                            used: &mut HashSet<String>| {
            for (attacker, _, at_me) in &attackers {
                if !at_me
                    || blocked.contains(&attacker.attacker_id)
                    || !Self::life_in_danger(life, unblocked_damage(blocked), serious)
                {
                    continue;
                }
                let need = attacker.min_blockers.max(1) as usize;
                let picks = candidates(attacker, used)
                    .into_iter()
                    .take(need)
                    .collect::<Vec<_>>();
                if picks.len() < need {
                    continue;
                }
                for blocker in picks {
                    assign(&attacker.attacker_id, blocker, assignments, blocked, used);
                }
            }
        };

        good_blocks(&mut assignments, &mut blocked, &mut used);
        gang_blocks(&mut assignments, &mut blocked, &mut used);
        let danger = Self::life_in_danger(life, unblocked_damage(&blocked), false);
        trade_blocks(danger, &mut assignments, &mut blocked, &mut used);
        if danger {
            chump_blocks(false, &mut assignments, &mut blocked, &mut used);
        }
        if Self::life_in_danger(life, unblocked_damage(&blocked), true) {
            assignments.clear();
            blocked.clear();
            used.clear();
            chump_blocks(true, &mut assignments, &mut blocked, &mut used);
            trade_blocks(true, &mut assignments, &mut blocked, &mut used);
            good_blocks(&mut assignments, &mut blocked, &mut used);
            gang_blocks(&mut assignments, &mut blocked, &mut used);
        }
        for (attacker, _, _) in &attackers {
            if !attacker.must_be_blocked || blocked.contains(&attacker.attacker_id) {
                continue;
            }
            let need = attacker.min_blockers.max(1) as usize;
            let picks = candidates(attacker, &used)
                .into_iter()
                .take(need)
                .collect::<Vec<_>>();
            if picks.len() == need {
                for blocker in picks {
                    assign(
                        &attacker.attacker_id,
                        blocker,
                        &mut assignments,
                        &mut blocked,
                        &mut used,
                    );
                }
            }
        }
        assignments
    }

    fn damage_amount(text: &str) -> Option<i32> {
        let lower = text.to_ascii_lowercase();
        let at = lower.find("deals ")?;
        let rest = &lower[at + "deals ".len()..];
        let number = rest.split(' ').next()?;
        if !rest.contains("damage") {
            return None;
        }
        number.parse().ok()
    }

    // Recompute from the current stack each prompt, so countered, removed or
    // retargeted objects cannot leave persistent reservations. This is only a
    // prospective estimate; prevention, replacements and future play differ.
    fn pending_damage(&self, player_id: &str, target_id: &str) -> i32 {
        let Some(view)=&self.view else { return 0; };
        view.stack.iter().enumerate().filter_map(|(i,item)| {
            if item.controller_id!=player_id || item.is_casting {return None;}
            let amount=item.known_damage.or_else(|| item.source_ability_text.as_deref().or(Some(item.text.as_str())).and_then(Self::damage_amount))?;
            if amount<=0 || item.targets.len()!=1 || !item.targets.iter().any(|t|
                matches!(t.kind,TargetKind::Player | TargetKind::Card) && t.id==target_id && matches!(t.intent,Some(TargetingIntent::Damage))) {return None;}
            if view.stack[i+1..].iter().any(|threat|threat.targets.iter().any(|t|
                t.id==item.id && matches!(t.kind,TargetKind::Spell))) {return None;}
            Some(amount)
        }).fold(0i32,|sum,amount|sum.saturating_add(amount))
    }

    /// Own abilities and spells on the stack that are not already countered (a later spell
    /// or ability of anyone targeting them).
    fn own_pending_items(&self, player_id: &str) -> Vec<&manabrew_agent_interface::game_view_dto::StackObjectDto> {
        self.side_pending_items(player_id, player_id)
    }

    /// Whether a stack object of `controller` works for us against an object `target_controller`
    /// controls: ours, or, when the object is not ours, any other seat's. A second opponent's
    /// counter or removal at the same object does what ours would.
    fn on_our_side(player_id: &str, target_controller: &str, controller: &str) -> bool {
        controller == player_id || (target_controller != player_id && controller != target_controller)
    }

    /// Pending (not already countered) spells and abilities on our side against an object that
    /// `target_controller` controls, see `on_our_side`.
    fn side_pending_items(&self, player_id: &str, target_controller: &str) -> Vec<&manabrew_agent_interface::game_view_dto::StackObjectDto> {
        let Some(view) = &self.view else { return Vec::new(); };
        view.stack.iter().enumerate().filter(|(i, item)| {
            Self::on_our_side(player_id, target_controller, &item.controller_id) && !item.is_casting
                && !view.stack[i + 1..].iter().any(|later| later.targets.iter().any(|t|
                    t.id == item.id && matches!(t.kind, TargetKind::Spell)))
        }).map(|(_, item)| item).collect()
    }

    /// Permanents or cards our side's stack already destroys, exiles, bounces or blinks (ours,
    /// and for an opponent's permanent any other opponent's too). A second removal aimed at one
    /// would fizzle once the first resolves.
    fn removed_by_stack(&self, player_id: &str, target_id: &str) -> bool {
        let controller = self.card(target_id).map_or(player_id, |card| card.controller_id.as_str());
        self.side_pending_items(player_id, controller).iter().any(|item| item.targets.iter().any(|t|
            t.id == target_id && matches!(t.kind, TargetKind::Card)
                && matches!(t.intent, Some(TargetingIntent::Destroy | TargetingIntent::Exile | TargetingIntent::Bounce))))
    }

    /// Two of our pending objects take, remove or counter the same target, the usual state of a
    /// spell and its copy: whichever resolves second would fizzle, so the copy picks anew.
    fn own_targets_overlap(&self, player_id: &str) -> bool {
        let acts = |t: &TargetRef| matches!(t.intent, Some(TargetingIntent::Damage | TargetingIntent::Destroy
            | TargetingIntent::Exile | TargetingIntent::Bounce | TargetingIntent::Counter));
        let items = self.own_pending_items(player_id);
        items.iter().enumerate().any(|(i, item)| item.targets.iter().filter(|t| acts(t)).any(|t|
            items[i + 1..].iter().any(|other| other.targets.iter().any(|o| acts(o) && o.id == t.id))))
    }

    /// How many of our pending damage objects aim at this target, whatever they deal.
    fn own_damage_hits(&self, player_id: &str, target_id: &str) -> i32 {
        self.own_pending_items(player_id).iter().filter(|item| item.targets.iter().any(|t|
            t.id == target_id && matches!(t.intent, Some(TargetingIntent::Damage)))).count() as i32
    }

    fn damage_target_score(&self, target: &TargetRef, player_id: &str, damage: i32) -> i32 {
        match target.kind {
            TargetKind::Player => {
                if target.id == player_id {
                    return -1_000;
                }
                let life = self
                    .view
                    .as_ref()
                    .and_then(|view| view.players.iter().find(|player| player.id == target.id))
                    .map_or(20, |player| player.life);
                if damage >= life {
                    10_000
                } else {
                    1_000 + self.attack_target_score(&target.id, player_id) / 8
                }
            }
            TargetKind::Card => {
                let Some(card) = self.card(&target.id) else {
                    return 0;
                };
                let importance = self.importance(card);
                if card.controller_id == player_id {
                    return -importance;
                }
                let lethal = (Self::stat(card.toughness.as_deref()) - card.damage).max(1);
                if card.types.iter().any(|ty| ty == "Creature") && lethal > damage {
                    importance / 4
                } else {
                    1_000 + importance * 4
                }
            }
            TargetKind::Spell => 0,
        }
    }

    // Suppress only a narrowly established redundant voluntary answer. Unknown,
    // conditional, threatened or potentially illegal answers remain available.
    fn covered_by_friendly_answer(&self, target: &TargetRef, player_id: &str) -> bool {
        let Some(view) = &self.view else { return false; };
        if !matches!(target.kind, TargetKind::Spell | TargetKind::Card) { return false; }
        // Whose object this is decides whose answers count as ours (`on_our_side`).
        let target_controller;
        if matches!(target.kind, TargetKind::Spell) {
            let Some(object) = view.stack.iter().find(|item| item.id == target.id && !item.is_casting) else { return false; };
            target_controller = object.controller_id.as_str();
            // Two or more of our counters at one spell are enough, even taxing ones (storm
            // copies of Flusterstorm); a further counter would only fizzle after it is taken.
            if self.side_pending_items(player_id, target_controller).iter().filter(|item| item.targets.iter().any(|t|
                t.id == target.id && matches!(t.intent, Some(TargetingIntent::Counter)))).count() >= 2 {
                return true;
            }
            if object.source_card.as_ref().is_none_or(|c| c.text.to_ascii_lowercase().contains("can't be countered") || c.text.to_ascii_lowercase().contains("cannot be countered")) { return false; }
        } else {
            let Some(card) = self.card(&target.id) else { return false; };
            target_controller = card.controller_id.as_str();
            if card.zone != Some(ZoneKind::Battlefield) || card.keywords.iter().any(|k| {
                let k=k.to_ascii_lowercase(); k.contains("shroud") || k.contains("hexproof") || k.contains("protection")
            }) { return false; }
        }
        view.stack.iter().enumerate().any(|(index, answer)| {
            if !Self::on_our_side(player_id, target_controller, &answer.controller_id) || answer.is_casting { return false; }
            let effect=answer.source_ability_text.as_deref().unwrap_or(&answer.text).to_ascii_lowercase();
            if ["unless", "you may", "if you", "if that", "indestructible"].iter().any(|word| effect.contains(word)) { return false; }
            let clause = effect.split('.').next().unwrap_or("").trim();
            let sufficient = answer.targets.iter().any(|t| {
                if t.id != target.id || t.kind != target.kind { return false; }
                match (t.intent, target.kind) {
                    (Some(TargetingIntent::Counter), TargetKind::Spell) => {
                        let Some(object)=view.stack.iter().find(|item| item.id==t.id) else { return false; };
                        let Some(card)=object.source_card.as_ref() else { return false; };
                        clause.starts_with("counter target ") && Self::counter_clause_allows(clause, &card.types, false)
                    }
                    (Some(intent @ (TargetingIntent::Destroy | TargetingIntent::Exile | TargetingIntent::Bounce)), TargetKind::Card) => {
                        let Some(card)=self.card(&t.id) else { return false; };
                        if matches!(intent,TargetingIntent::Destroy) && card.keywords.iter().any(|k|k.eq_ignore_ascii_case("indestructible")) { return false; }
                        let restriction = match intent {
                            TargetingIntent::Destroy => clause.strip_prefix("destroy target "),
                            TargetingIntent::Exile => clause.strip_prefix("exile target "),
                            TargetingIntent::Bounce => clause.strip_prefix("return target ").and_then(|s|s.strip_suffix(" to its owner's hand"))
                                .or_else(||clause.strip_prefix("put target ").and_then(|s|s.strip_suffix(" on top of its owner's library"))),
                            _ => None,
                        };
                        match restriction {
                            Some("creature") => card.types.iter().any(|ty|ty=="Creature"),
                            Some("nonblack creature") => !card.color.contains('B') && card.types.iter().any(|ty|ty=="Creature"),
                            Some("artifact") => card.types.iter().any(|ty|ty=="Artifact"),
                            Some("nonland permanent") => !card.types.iter().any(|ty|ty=="Land"),
                            _ => false,
                        }
                    }
                    _ => false,
                }
            });
            if !sufficient { return false; }
            // Stack is in resolution order bottom to top. A later counter/removal from another
            // seat than the answer's own, aimed at the answer, may save the original.
            !view.stack[index+1..].iter().any(|threat| threat.controller_id != answer.controller_id
                && threat.targets.iter().any(|t| t.id == answer.id && matches!(t.intent,
                    Some(TargetingIntent::Counter | TargetingIntent::Exile | TargetingIntent::Bounce))))
        })
    }

    fn target_score(&self, target: &TargetRef, player_id: &str, prompt_hostile: bool) -> i32 {
        let hostile = prompt_hostile
            || matches!(
                target.intent,
                Some(
                    TargetingIntent::Damage
                        | TargetingIntent::Destroy
                        | TargetingIntent::Sacrifice
                        | TargetingIntent::Exile
                        | TargetingIntent::Bounce
                        | TargetingIntent::Mill
                        | TargetingIntent::Discard
                        | TargetingIntent::Counter
                        | TargetingIntent::Tap
                        | TargetingIntent::Debuff
                        | TargetingIntent::LoseLife
                        | TargetingIntent::GainControl
                        | TargetingIntent::Fight
                        | TargetingIntent::Hostile
                )
            );
        match target.kind {
            TargetKind::Player => {
                let value = self.attack_target_score(&target.id, player_id);
                let own = target.id == player_id;
                if hostile == own {
                    -value
                } else {
                    value + 1_000
                }
            }
            TargetKind::Card => {
                let Some(card) = self.card(&target.id) else {
                    return 0;
                };
                let mut value = self.importance(card) + if !hostile && self.life_counter_pinger(player_id, false).is_some_and(|pinger| pinger.id == card.id) { 10_000 } else { 0 };
                let own = card.controller_id == player_id;
                // A helpful effect (protection, a blink) goes to the permanent under attack first.
                if !hostile && own && self.view.as_ref().is_some_and(|view| threat::threatened(view, card, player_id)) {
                    value += 5_000;
                }
                if hostile == own {
                    -value
                } else {
                    value + 1_000
                }
            }
            TargetKind::Spell => self
                .view
                .as_ref()
                .and_then(|view| view.stack.iter().find(|item| item.id == target.id))
                .map_or(0, |item| {
                    if item.controller_id == player_id {
                        -1_000
                    } else {
                        1_000 + self.view.as_ref().map_or(0, |view| threat::spell_importance(view, item, player_id))
                    }
                }),
        }
    }

    /// The players our attacking creatures are attacking, or whose planeswalkers and battles they
    /// are: who "that player" can be for a trigger that follows our combat damage.
    fn players_reached_in_combat(&self, me: &str) -> Vec<String> {
        let mut reached: Vec<String> = Vec::new();
        for card in self.battlefield(me).filter(|card| card.is_attacking) {
            let Some(target) = card.attack_target_id.as_deref() else { continue };
            let player = self.card(target).map_or_else(|| target.to_string(), |permanent| permanent.controller_id.clone());
            if player != me && !reached.contains(&player) {
                reached.push(player);
            }
        }
        reached
    }

    /// How much `me` should want to hit this attack target: an opponent's danger, a planeswalker's importance.
    fn attack_target_score(&self, id: &str, me: &str) -> i32 {
        let Some(view) = &self.view else {
            return 0;
        };
        if view.players.iter().any(|player| player.id == id) {
            return threat::opponent_danger(view, id, me);
        }
        self.card(id).map_or(0, |card| self.importance(card))
    }

    fn importance(&self, card: &CardDto) -> i32 {
        self.view.as_ref().map_or_else(|| Self::card_value(card), |view| threat::permanent_importance(view, card))
    }
}

impl BotAgent for SimpleAi {
    fn observe(&mut self, view: GameViewDto) {
        self.pending_view = None;
        self.has_command_cards |= view
            .zones
            .iter()
            .any(|zone| zone.zone == ZoneKind::Command && zone.count > 0);
        if self.turn != Some(view.turn) {
            self.turn = Some(view.turn);
            self.attempted_actions.clear();
            self.last_lifelink_combo_progress = None;
            self.pending_lifelink_combo_cost = None;
            self.last_tutor_sacrifice_state = None;
            self.known_tutor_targets = None;
            self.payment_attempt = None;
            self.payment_actions.clear();
            self.payment_state = None;
            self.recent_prompts.clear();
            self.failed_attack_targets.clear();
            self.paid_life_round = None;
        }
        self.card_locations.clear();
        for (zone_index, zone) in view.zones.iter().enumerate() {
            for (card_index, card) in zone.cards.iter().enumerate() {
                if let CardView::Visible(card) = card {
                    if card.is_double_faced && !card.is_transformed && Self::has_entry_ability(&card.text) {
                        self.front_face_entry.insert(card.id.clone());
                    }
                    self.card_locations
                        .insert(card.id.clone(), (zone_index, card_index));
                }
            }
        }
        self.view = Some(view);
    }

    fn decide(&mut self, prompt: AgentPrompt) -> Option<PromptOutput> {
        if Self::needs_view(&prompt.input) {
            self.ensure_view();
        }
        let deciding_player_id = prompt.deciding_player_id.clone();
        let prompt_source_id = prompt
            .source_card
            .as_ref()
            .map_or_else(|| "none".to_string(), |card| card.id.clone());
        let prompt_card_text = prompt.source_card.as_ref().map(|card| card.text.to_ascii_lowercase()).unwrap_or_default();
        let prompt_source_name = prompt.source_card.as_ref().map(|card| card.identity.name.clone()).unwrap_or_default();
        let pending_hand_refill = self.pending_hand_refill_source.take();
        let prompt_source_text = prompt
            .source_ability_text
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match prompt.input {
        PromptInput::Mulligan(manabrew_protocol::prompts::mulligan::MulliganInput {
                hand_card_ids,
                mulligan_count,
            }) => {
                let lands = hand_card_ids
                    .iter()
                    .filter_map(|card_id| self.card(card_id))
                    .filter(|card| card.types.iter().any(|card_type| card_type == "Land"))
                    .count();
                let keep = mulligan_count >= 2 || (2..=5).contains(&lands);
                Some(PromptOutput::Mulligan(MulliganOutput::MulliganDecision { keep }))
            }
            PromptInput::MulliganPutBack(manabrew_protocol::prompts::mulligan_put_back::MulliganPutBackInput {
                mut hand_card_ids,
                count,
                ..
            }) => {
                let lands = hand_card_ids
                    .iter()
                    .filter_map(|card_id| self.card(card_id))
                    .filter(|card| card.types.iter().any(|card_type| card_type == "Land"))
                    .count();
                hand_card_ids.sort_by_key(|card_id| {
                    let Some(card) = self.card(card_id) else {
                        return 0;
                    };
                    if lands > 3 && card.types.iter().any(|card_type| card_type == "Land") {
                        -1
                    } else {
                        Self::card_value(card)
                    }
                });
                Some(PromptOutput::MulliganPutBack(MulliganPutBackOutput::MulliganPutBackDecision {
                    card_ids: hand_card_ids.into_iter().take(count).collect(),
                }))
            }
            PromptInput::ChooseAction(manabrew_protocol::prompts::choose_action::ChooseActionInput { actions }) => {
                self.awaiting_mana_color = false;
                self.pending_mana_color = None;
                self.auto_payment_active = false;
                self.selected_mana_source = None;
                // The cast chosen last time never reached the stack: the engine could not
                // start it. Do not start it again against this same board.
                if let Some((card_id, stack_before)) = self.last_cast_pick.take() {
                    let failed = self.view.as_ref().is_some_and(|view| view.stack.len() <= stack_before)
                        && self.card(&card_id).is_some_and(|card| {
                            card.zone == Some(ZoneKind::Hand) && card.controller_id == deciding_player_id
                        });
                    if failed {
                        self.note_cancelled_play(&card_id);
                    }
                }
                if let Some(target) = self.death_tutor_plan_target(&deciding_player_id) {
                    let state=format!("{}:{:?}:{}:{}", target.id, target.counters,
                        self.view.as_ref().and_then(|v|v.players.iter().find(|p|p.id==deciding_player_id)).map(|p|p.life).unwrap_or(0),
                        self.battlefield(&deciding_player_id).map(|c|c.id.as_str()).collect::<Vec<_>>().join(","));
                    if self.last_tutor_sacrifice_state.as_ref().is_some_and(|prior| prior != &state) {
                        for action in &actions { if let AvailableActionKind::ActivateAbility(info)=&action.kind {
                            if info.cost.as_deref().is_some_and(|cost|cost.to_ascii_lowercase().contains("sac")) {
                                self.attempted_actions.remove(&Self::action_key(action));
                            }
                        }}
                    }
                    self.last_tutor_sacrifice_state=Some(state);
                }
                if let Some(pinger_id) = self.life_counter_pinger(&deciding_player_id, true).filter(|card| card.counters.values().any(|count| *count >= 2)).map(|c| c.id.clone()) {
                    let progress = self.view.as_ref().map(|v| v.players.iter().filter(|p| p.id != deciding_player_id).map(|p| p.life.max(0)).sum::<i32>()).unwrap_or(0);
                    if self.last_lifelink_combo_progress.is_some_and(|previous| progress < previous) {
                        self.attempted_actions.retain(|key| !key.starts_with(&format!("ability:{pinger_id}:")));
                    }
                    self.last_lifelink_combo_progress = Some(progress);
                }
                self.reserve = self.plan_reserve(&actions, &deciding_player_id);
                let pick = actions
                    .iter()
                    .filter(|action| {
                        !matches!(&action.kind, AvailableActionKind::UndoMana { .. })
                            && !matches!(
                                &action.kind,
                                AvailableActionKind::ActivateAbility(info) if info.is_mana_ability
                            )
                            && !self.attempted_actions.contains(&Self::action_key(action))
                            && !matches!(
                                &action.kind,
                                AvailableActionKind::ActivateAbility(info) if self.wasted_activation(info)
                            )
                            && self.sane(action, &deciding_player_id)
                            && !self.dips_into_reserve(action, &deciding_player_id)
                    })
                    .max_by_key(|action| self.action_score(action, &deciding_player_id));
                self.pending_lifelink_combo_cost = pick.and_then(|action| match &action.kind {
                    AvailableActionKind::ActivateAbility(info) if self.life_counter_pinger(&deciding_player_id, true).is_some_and(|card| card.id == info.card_id) => Some(info.card_id.clone()),
                    _ => None,
                });
                self.pending_voluntary_cast = pick.and_then(|action| match &action.kind {
                    AvailableActionKind::Cast { card_id, .. } => Some(card_id.clone()),
                    _ => None,
                });
                self.last_cast_pick = pick.and_then(|action| match &action.kind {
                    AvailableActionKind::Cast { card_id, .. } if !self.is_land_play(action) => {
                        Some((card_id.clone(), self.view.as_ref().map_or(0, |view| view.stack.len())))
                    }
                    _ => None,
                });
                let pick = pick.map(|action| {
                    (action.id.clone(), Self::action_key(action))
                });
                self.payment_attempt = None;
                self.payment_actions.clear();
                self.payment_state = None;
                if let Some((_, key)) = &pick {
                    self.attempted_actions.insert(key.clone());
                }
                Some(PromptOutput::ChooseAction(match pick {
                    Some((action_id, _)) => ChooseActionOutput::Act { action_id },
                    None => ChooseActionOutput::Pass {
                        until: Some(PassUntil {
                            player_id: deciding_player_id.clone(),
                            phase: manabrew_protocol::game::StepKind::Main1,
                            through_combat: false,
                        }),
                        exhaust_stack: true,
                    },
                }))
            }
            PromptInput::ChooseAttackers(manabrew_protocol::prompts::choose_attackers::ChooseAttackersInput {
                attackers,
                attack_targets,
                max_attackers,
                target_limits,
            }) => {
                let default_target = attack_targets
                    .iter()
                    .max_by_key(|target| self.attack_target_score(&target.id, &deciding_player_id))
                    .map(|target| target.id.clone())
                    .unwrap_or_else(|| "player-1".to_string());
                let attack_power = attackers
                    .iter()
                    .filter_map(|attacker| self.card(&attacker.attacker_id))
                    .filter_map(|card| card.power.as_deref())
                    .filter_map(|power| power.parse::<i32>().ok())
                    .sum::<i32>();
                let lethal_target = self.view.as_ref().is_some_and(|view| {
                    view.players
                        .iter()
                        .find(|player| player.id == default_target)
                        .is_some_and(|player| player.life > 0 && attack_power >= player.life)
                });
                let signature = format!(
                    "attack:{}|{}",
                    attackers
                        .iter()
                        .map(|a| format!("{}:{}", a.attacker_id, a.valid_target_ids.join("+")))
                        .collect::<Vec<_>>()
                        .join(","),
                    attack_targets
                        .iter()
                        .map(|t| t.id.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                );
                let reprompted = self.looping_on_consecutive(signature);
                let keep = if lethal_target {
                    HashSet::new()
                } else {
                    self.blockers_to_keep(
                        &deciding_player_id,
                        &attackers.iter().map(|a| a.attacker_id.clone()).collect::<Vec<_>>(),
                    )
                };
                // Minimum-cost maximum matching satisfies as many required
                // attacks as possible, then minimizes avoidable attacks at the
                // goader/non-player targets across the entire declaration.
                let mut required: Vec<_> = attackers.iter().filter(|a| a.must_attack).collect();
                required.sort_by_key(|a| a.valid_target_ids.len());
                #[derive(Clone)]
                struct Edge { to: usize, rev: usize, cap: i32, cost: i32 }
                fn add_edge(graph: &mut [Vec<Edge>], from: usize, to: usize, cap: i32, cost: i32) {
                    let rev_to = graph[to].len(); let rev_from = graph[from].len();
                    graph[from].push(Edge { to, rev: rev_to, cap, cost });
                    graph[to].push(Edge { to: from, rev: rev_from, cap: 0, cost: -cost });
                }
                let source = 0; let attacker_base = 1; let target_base = attacker_base + required.len();
                let sink = target_base + attack_targets.len();
                let mut graph: Vec<Vec<Edge>> = vec![Vec::new(); sink + 1];
                for (index, attacker) in required.iter().enumerate() {
                    add_edge(&mut graph, source, attacker_base + index, 1, 0);
                    let mut targets: Vec<_> = attacker.valid_target_ids.iter()
                        .filter(|target| !self.failed_attack_targets.contains(*target)).collect();
                    targets.sort_by_key(|target| std::cmp::Reverse((attacker.goaded_by.as_ref().is_some_and(|g|
                        target.starts_with("player-") && *target != g), self.attack_target_score(target, &deciding_player_id))));
                    for target in targets {
                        if let Some(position) = attack_targets.iter().position(|item| item.id == *target) {
                            let penalty = i32::from(attacker.goaded_by.as_ref().is_some_and(|g|
                                !target.starts_with("player-") || target == g));
                            add_edge(&mut graph, attacker_base + index, target_base + position, 1, penalty);
                        }
                    }
                }
                for (index, target) in attack_targets.iter().enumerate() {
                    let cap = target_limits.get(&target.id).copied().unwrap_or(required.len()).min(required.len());
                    add_edge(&mut graph, target_base + index, sink, cap as i32, 0);
                }
                let maximum = max_attackers.unwrap_or(required.len()).min(required.len());
                for _ in 0..maximum {
                    let mut distance = vec![i32::MAX; graph.len()];
                    let mut previous: Vec<Option<(usize, usize)>> = vec![None; graph.len()];
                    distance[source] = 0;
                    for _ in 0..graph.len() {
                        let mut changed = false;
                        for node in 0..graph.len() {
                            if distance[node] == i32::MAX { continue; }
                            for (edge_id, edge) in graph[node].iter().enumerate() {
                                let next = distance[node] + edge.cost;
                                if edge.cap > 0 && next < distance[edge.to] {
                                    distance[edge.to] = next; previous[edge.to] = Some((node, edge_id)); changed = true;
                                }
                            }
                        }
                        if !changed { break; }
                    }
                    if previous[sink].is_none() { break; }
                    let mut node = sink;
                    while node != source {
                        let (prior, edge_id) = previous[node].expect("reachable matching path");
                        let reverse = graph[prior][edge_id].rev;
                        graph[prior][edge_id].cap -= 1;
                        graph[node][reverse].cap += 1;
                        node = prior;
                    }
                }
                let mut assignments: Vec<AttackAssignment> = Vec::new();
                for (index, attacker) in required.iter().enumerate() {
                    if let Some(edge) = graph[attacker_base + index].iter()
                        .find(|edge| edge.to >= target_base && edge.to < sink && edge.cap == 0) {
                        assignments.push(AttackAssignment { attacker_id: attacker.attacker_id.clone(),
                            target_id: attack_targets[edge.to - target_base].id.clone() });
                    }
                }
                // Optional attacks use only capacity left after requirements.
                for a in attackers.iter().filter(|a| !a.must_attack) {
                    if max_attackers.is_some_and(|max| assignments.len() >= max) { break; }
                    if reprompted || keep.contains(&a.attacker_id) { continue; }
                    // The most dangerous opponent this creature can safely and usefully attack.
                    let mut targets: Vec<&String> = a.valid_target_ids.iter()
                        .filter(|target| !self.failed_attack_targets.contains(*target))
                        .filter(|target| target_limits.get(*target).is_none_or(|limit|
                            assignments.iter().filter(|entry| entry.target_id.as_str() == target.as_str()).count() < *limit))
                        .collect();
                    targets.sort_by_key(|target| std::cmp::Reverse(self.attack_target_score(target, &deciding_player_id)));
                    let Some(target_id) = targets.into_iter()
                        .find(|target| (lethal_target && **target == default_target) || self.should_attack(&a.attacker_id, target))
                        .cloned() else { continue };
                    assignments.push(AttackAssignment { attacker_id: a.attacker_id.clone(), target_id });
                }
                self.last_attack_declaration = assignments
                    .iter()
                    .map(|a| (a.attacker_id.clone(), a.target_id.clone()))
                    .collect();
                Some(PromptOutput::ChooseAttackers(ChooseAttackersOutput::DeclareAttackers {
                    assignments,
                }))
            }
            PromptInput::ChooseBlockers(manabrew_protocol::prompts::choose_blockers::ChooseBlockersInput {
                attackers,
                available_blocker_ids,
                ..
            }) => {
                let assignments =
                    self.declare_blockers(&attackers, &available_blocker_ids, &deciding_player_id);
                Some(PromptOutput::ChooseBlockers(ChooseBlockersOutput::DeclareBlockers { assignments }))
            }
            PromptInput::ChooseBoardTargets(manabrew_protocol::prompts::choose_board_targets::ChooseBoardTargetsInput {
                candidates, hostile, min_targets, max_targets, chosen_targets, presentation, cancellable, intent, ..
            }) => {
                let take = (max_targets - chosen_targets).max(min_targets - chosen_targets).max(0)
                    as usize;
                let damage = prompt.known_damage.or_else(|| prompt
                    .source_ability_text
                    .as_deref()
                    .and_then(Self::damage_amount)
                    .or_else(|| prompt.source_card.as_ref().and_then(|card| Self::damage_amount(&card.text))));
                let mut candidates = candidates;
                let voluntary = cancellable && hostile && min_targets == 1 && max_targets == 1
                    && prompt.source_card.as_ref().is_some_and(|card| self.pending_voluntary_cast.as_deref() == Some(card.id.as_str()));
                // TargetingIntent::Counter is also used by ChangeTargets spells;
                // never treat a redirect or another action as a counter effect.
                let effect = prompt.source_ability_text.as_deref()
                    .or_else(|| prompt.source_card.as_ref().map(|card| card.text.as_str()))
                    .unwrap_or("").trim().to_ascii_lowercase();
                let direct_answer = match intent {
                    TargetingIntent::Counter => effect.starts_with("counter "),
                    TargetingIntent::Destroy => effect.starts_with("destroy "),
                    TargetingIntent::Exile => effect.starts_with("exile "),
                    TargetingIntent::Bounce => effect.starts_with("return ") || effect.starts_with("put target "),
                    _ => false,
                };
                let voluntary_cast = cancellable && prompt.source_card.as_ref()
                    .is_some_and(|card| self.pending_voluntary_cast.as_deref() == Some(card.id.as_str()));
                let required = (min_targets - chosen_targets).max(0) as usize;
                let own_card = |target: &TargetRef| matches!(target.kind, TargetKind::Card)
                    && self.card(&target.id).is_some_and(|card| card.controller_id == deciding_player_id);
                // Exiling the player's own permanent and returning it at once is
                // reuse, not removal: the engine labels it hostile `Exile`.
                let self_reuse = matches!(intent, TargetingIntent::Exile)
                    && effect.contains("return") && effect.contains("to the battlefield")
                    && !candidates.is_empty() && candidates.iter().all(|target| own_card(target));
                // "Destroy ... its controller creates two tokens that are copies":
                // a self-copy effect, which hands opponents two copies if aimed at them.
                let self_copy = matches!(intent, TargetingIntent::Destroy)
                    && effect.contains("creates two tokens that are copies");
                if self_reuse || self_copy {
                    let mut scored: Vec<_> = candidates.iter().map(|target| {
                        let card = self.card(&target.id);
                        let value = card.and_then(|card| self.reuse_value(card, &deciding_player_id, prompt.source_card.as_ref()));
                        (value, -card.map_or(0, Self::card_value), target.clone())
                    }).collect();
                    scored.sort_by_key(|(value, low, _)| std::cmp::Reverse((value.unwrap_or(i32::MIN), *low)));
                    let useful = scored.iter().filter(|(value, _, _)| value.is_some_and(|v| v > 0)).count();
                    if useful == 0 && voluntary_cast {
                        self.pending_voluntary_cast = None;
                        self.note_cancelled_play(&prompt_source_id);
                        return Some(PromptOutput::ChooseBoardTargets(ChooseBoardTargetsOutput::Cancel));
                    }
                    if self_reuse || useful > 0 {
                        let chosen: Vec<_> = scored.into_iter().take(take.min(useful).max(required))
                            .map(|(_, _, target)| target).collect();
                        if self_reuse {
                            self.pending_reuse_target = chosen.first().map(|target| (prompt_source_id.clone(), target.id.clone()));
                        }
                        return Some(PromptOutput::ChooseBoardTargets(ChooseBoardTargetsOutput::BoardTargets { chosen }));
                    }
                }
                // Reanimation Auras have no API and arrive labeled hostile; value the
                // returned creature for its new controller, whoever owns the card.
                if prompt_card_text.contains("enchant creature card in a graveyard")
                    && candidates.iter().any(|target| matches!(target.kind, TargetKind::Card)) {
                    let tribes: Vec<String> = self.battlefield(&deciding_player_id).filter_map(|card| {
                        let text = card.text.to_ascii_lowercase();
                        let rest = text.split("whenever another ").nth(1)?;
                        let tribe = rest.split(" you control enters").next()?;
                        (!tribe.contains(' ') && tribe != "creature").then(|| tribe.to_string())
                    }).collect();
                    let mut scored: Vec<_> = candidates.iter().map(|target| {
                        let score = self.card(&target.id).map_or(i32::MIN, |card| {
                            let tribal = card.subtypes.iter().any(|t| tribes.iter().any(|tribe| t.eq_ignore_ascii_case(tribe)));
                            Self::card_value(card) + if tribal { 25 } else { 0 }
                                + if card.owner_id == deciding_player_id { 2 } else { 0 }
                        });
                        (score, target.clone())
                    }).collect();
                    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
                    return Some(PromptOutput::ChooseBoardTargets(ChooseBoardTargetsOutput::BoardTargets {
                        chosen: scored.into_iter().take(take).map(|(_, target)| target).collect(),
                    }));
                }
                if voluntary && direct_answer {
                    // The same rule the cast check used to decide the spell was worth starting.
                    candidates.retain(|target| self.answer_target_worthwhile(target, intent, prompt.source_card.as_ref(), &deciding_player_id));
                    if candidates.is_empty() {
                        self.pending_voluntary_cast = None;
                        self.note_cancelled_play(&prompt_source_id);
                        return Some(PromptOutput::ChooseBoardTargets(ChooseBoardTargetsOutput::Cancel));
                    }
                }
                // "... target creature that player controls": the engine lists every creature, but the
                // player is the one our combat just reached. Only their permanents are legal picks.
                if ["that player controls", "defending player controls", "that opponent controls"].iter().any(|phrase| effect.contains(phrase)) {
                    let reached = self.players_reached_in_combat(&deciding_player_id);
                    if !reached.is_empty() {
                        let belongs = |target: &TargetRef| match target.kind {
                            TargetKind::Card => self.card(&target.id).is_some_and(|card| reached.contains(&card.controller_id)),
                            TargetKind::Player => reached.contains(&target.id),
                            TargetKind::Spell => false,
                        };
                        if candidates.iter().any(|target| belongs(target)) {
                            candidates.retain(|target| belongs(target));
                        } else if cancellable {
                            self.pending_voluntary_cast = None;
                            return Some(PromptOutput::ChooseBoardTargets(ChooseBoardTargetsOutput::Cancel));
                        }
                    }
                }
                candidates.sort_by_key(|target| {
                    let death_tutor = presentation.title.eq_ignore_ascii_case("Sacrifice")
                        && self.death_tutor_plan_target(&deciding_player_id).is_some_and(|card|card.id==target.id);
                    let base=if death_tutor { 100_000 } else { match damage {
                        Some(damage) if hostile => self.damage_target_score(target, &deciding_player_id, damage),
                        _ => self.target_score(target, &deciding_player_id, hostile),
                    }};
                    let countered = hostile && matches!(intent, TargetingIntent::Counter)
                        && matches!(target.kind, TargetKind::Spell)
                        && self.covered_by_friendly_answer(target, &deciding_player_id);
                    let covered = hostile && matches!(intent,TargetingIntent::Damage) && match target.kind {
                        TargetKind::Player if target.id!=deciding_player_id => self.view.as_ref().and_then(|view|view.players.iter().find(|p|p.id==target.id))
                            .is_some_and(|p| p.life>0 && self.pending_damage(&deciding_player_id,&target.id)>=p.life),
                        TargetKind::Card => self.card(&target.id).and_then(|card| card.toughness.as_deref().and_then(|n|n.parse::<i32>().ok()).map(|t| (t-card.damage).max(1)))
                            .is_some_and(|remaining| self.pending_damage(&deciding_player_id,&target.id)>=remaining),
                        _ => false,
                    };
                    let friendly = matches!(intent,TargetingIntent::Damage) && hostile && match target.kind {
                        TargetKind::Player => target.id == deciding_player_id,
                        TargetKind::Card => self.card(&target.id).is_some_and(|card| card.controller_id == deciding_player_id),
                        _ => false,
                    };
                    // Already dealt with by our own stack: a removal at a permanent already being
                    // removed, or damage at a target our other pending damage already covers or
                    // that is being removed. Picked only when nothing else is left.
                    let handled = hostile && matches!(target.kind, TargetKind::Card)
                        && matches!(intent, TargetingIntent::Damage | TargetingIntent::Destroy | TargetingIntent::Exile | TargetingIntent::Bounce)
                        && self.removed_by_stack(&deciding_player_id, &target.id);
                    let spread = if hostile && matches!(intent, TargetingIntent::Damage) { 2_000 * self.own_damage_hits(&deciding_player_id, &target.id) } else { 0 };
                    std::cmp::Reverse(base - if covered {100_000} else {0} - if friendly {200_000} else {0}
                        - if handled {300_000} else {0} - if countered {300_000} else {0} - spread)
                });
                Some(PromptOutput::ChooseBoardTargets(ChooseBoardTargetsOutput::BoardTargets {
                    chosen: candidates.into_iter().take(take).collect(),
                }))
            }
            PromptInput::Scry(manabrew_protocol::prompts::scry::ScryInput { cards, zones, .. }) => {
                let mut zone_card_ids = vec![Vec::new(); zones.len()];
                let away = zones.iter().position(|zone| {
                    matches!(
                        zone,
                        manabrew_protocol::prompts::scry::ScryDestination::LibraryBottom
                            | manabrew_protocol::prompts::scry::ScryDestination::Graveyard
                    )
                });
                let lands = self.lands_in_play(&deciding_player_id);
                for card in &cards {
                    let land = card.types.iter().any(|ty| ty == "Land");
                    let keep = if land { lands < 7 } else { card.cmc as usize <= lands + 2 };
                    let zone = if keep { 0 } else { away.unwrap_or(0) };
                    zone_card_ids[zone].push(card.id.clone());
                }
                Some(PromptOutput::Scry(ScryOutput::ScryDecision { zone_card_ids }))
            }
            PromptInput::RevealCards(manabrew_protocol::prompts::reveal::RevealCardsInput { .. }) => Some(PromptOutput::RevealCards(RevealCardsOutput::RevealCardsAcknowledged)),
            PromptInput::ChooseBoolean(manabrew_protocol::prompts::choose_boolean::ChooseBooleanInput {
                presentation,
                confirm_label,
                deny_label,
            }) => {
                let signature = format!(
                    "bool:{prompt_source_id}|{}|{confirm_label}|{deny_label}",
                    presentation.title
                );
                let repeated = self.looping_on(signature);
                let title = presentation.title.to_ascii_lowercase();
                // A copy of a damage or removal spell that keeps its original's target only repeats
                // it: the original fizzles once the copy has killed that target. New targets
                // are chosen by the same scoring, which already steers around damage that is
                // covered. A copy of a copy effect is not retargeted: it would copy itself forever.
                let retarget_copy = title.contains("choose new targets for the copy")
                    && self.view.as_ref().is_some_and(|view| {
                        let effect = |item: &manabrew_agent_interface::game_view_dto::StackObjectDto| item.source_ability_text.as_deref()
                            .filter(|t| !t.is_empty()).unwrap_or(&item.text).to_ascii_lowercase();
                        // Never while a copy spell is on the stack: retargeting copies of it
                        // would copy the copy again, without end.
                        !view.stack.iter().any(|item| item.is_spell && effect(item).contains("copy"))
                            && (view.stack.iter().any(|item| item.is_spell
                                && ["damage", "destroy", "exile"].iter().any(|word| effect(item).contains(word)))
                                || self.own_targets_overlap(&deciding_player_id))
                    });
                let always_accept = retarget_copy
                    || title.contains("cancel search")
                    || (title.contains("commander")
                        && title.contains("put it into the command zone"));
                let useful_single_draw = (title == "do you want to draw a card?"
                    || title == "do you want to draw 1 card(s)?"
                    || (confirm_label == "Accept" && title.ends_with(", you may draw a card.")))
                    && self.view.as_ref().is_some_and(|view| view.zones.iter().any(|zone| {
                        zone.zone == ZoneKind::Library && zone.owner_id == deciding_player_id && zone.count > 0
                    }));
                // Decline a hand replacement when it destroys a prepared hand,
                // but take a four-card refill from a depleted hand when the
                // library can supply it. This is not a general optional-effect rule.
                let useful_hand_refill = title.contains("discard your hand")
                    && title.contains("draw four cards")
                    && self.view.as_ref().is_some_and(|view| {
                        let count = |kind| view.zones.iter()
                            .find(|zone| zone.zone == kind && zone.owner_id == deciding_player_id)
                            .map(|zone| zone.count).unwrap_or(0);
                        count(ZoneKind::Hand) <= 1 && count(ZoneKind::Library) >= 4
                    });
                let pay_single_draw_tax = title == "pay {1} to prevent an opponent drawing a card?"
                    && self.view.as_ref().is_some_and(|view| {
                        let floating: u32 = view.players.iter().find(|p| p.id == deciding_player_id)
                            .map(|p| p.mana_pool.values().sum()).unwrap_or(0);
                        let untapped_lands = view.zones.iter().filter(|z| z.zone == ZoneKind::Battlefield)
                            .flat_map(|z| &z.cards).filter(|c| matches!(c,
                                CardView::Visible(card) if card.controller_id == deciding_player_id
                                    && !card.tapped && card.types.iter().any(|kind| kind == "Land"))).count();
                        floating > 0 || untapped_lands >= 2
                    });
                let constructed_duel = self.is_constructed_duel();
                let duel_cost = title.starts_with("pay 1 life")
                    || title.starts_with("pay 2 life")
                    || title.starts_with("pay {e}")
                    || title.starts_with("pay return an artifact")
                    || title.starts_with("sacrifice ");
                let own_activation_cost = (title.starts_with("pay ") || title.starts_with("sacrifice "))
                    && (self
                        .attempted_actions
                        .iter()
                        .any(|key| key.starts_with(&format!("ability:{prompt_source_id}:")))
                        || (title.starts_with("sacrifice ") && self.payment_attempt.is_some()));
                let life = self.view.as_ref().and_then(|view| {
                    view.players
                        .iter()
                        .find(|player| player.id == deciding_player_id)
                        .map(|player| player.life)
                });
                let untapped_land = presentation
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("enters tapped"))
                    && title.starts_with("pay ")
                    && life.is_some_and(|life| life >= 20);
                let accept_once = title.contains("search your library?")
                    || (constructed_duel && duel_cost)
                    || own_activation_cost
                    || untapped_land
                    || title.contains("sacrifice evolving wilds")
                    || title.contains("sacrifice bountiful landscape")
                    || title.contains("sacrifice strip mine")
                    || title.contains("exile simian spirit guide")
                    || title.starts_with("use triggered ability");
                let refill_cost = confirm_label == "Pay" && title.starts_with("pay cost for ")
                    && prompt_source_id != "none"
                    && pending_hand_refill.as_deref() == Some(prompt_source_id.as_str());
                let combo_cost = confirm_label == "Pay" && title.starts_with("pay cost for ")
                    && self.pending_lifelink_combo_cost.take().as_deref() == Some(prompt_source_id.as_str());
                let safe_copy_kicker = confirm_label == "Pay Kicker" && title == "pay kicker?"
                    && self.card(&prompt_source_id).is_some_and(|card| card.text.to_ascii_lowercase().contains("create five of those tokens instead"))
                    && self.battlefield(&deciding_player_id).any(|card| {
                        let text=card.text.to_ascii_lowercase();
                        text.contains("you draw x cards") && text.contains("you lose x life") && text.contains("number of demons you control")
                    })
                    && self.view.as_ref().is_some_and(|view| {
                        let demons=self.battlefield(&deciding_player_id).filter(|card| card.subtypes.iter().any(|t|t=="Demon")).count() as i32;
                        let legend_rule_disabled = self.battlefield(&deciding_player_id).any(|card| {
                            let text = card.text.to_ascii_lowercase();
                            text.contains("legend rule") && (text.contains("doesn't apply") || text.contains("does not apply"))
                        });
                        let upper_bound=5*(demons + if legend_rule_disabled {5} else {0});
                        let life=view.players.iter().find(|p|p.id==deciding_player_id).map(|p|p.life).unwrap_or(0);
                        let library=view.zones.iter().find(|z|z.owner_id==deciding_player_id && z.zone==ZoneKind::Library).map(|z|z.count as i32).unwrap_or(0);
                        life>upper_bound && library>=upper_bound
                    });
                let mana_payment_context = self.auto_payment_active
                    || self.selected_mana_source.as_deref() == Some(prompt_source_id.as_str());
                let life_cost = prompt_card_text.split("pay ").skip(1).filter_map(|part| {
                    let mut words=part.split_whitespace();
                    let amount=words.next()?.parse::<i32>().ok()?;
                    words.next().filter(|word| word.trim_matches(|c: char| !c.is_alphabetic()) == "life")?;
                    (amount > 0).then_some(amount)
                }).max();
                let title_life_cost = title.strip_prefix("pay ").and_then(|rest| rest.split_whitespace().next())
                    .and_then(|amount| amount.parse::<i32>().ok()).filter(|amount| *amount > 0);
                let safe_life_payment = life_cost.or(title_life_cost).zip(life).is_some_and(|(amount, current)| current > amount);
                let mana_cost_confirmation = mana_payment_context && confirm_label == "Pay" && prompt_source_id != "none"
                    && (matches!(title.as_str(), "remove counter for mana" | "exile for mana" | "sacrifice for mana")
                        || (title.starts_with("pay cost for ") && self.selected_mana_source.as_deref() == Some(prompt_source_id.as_str()))
                        || ((title == "pay life for mana" || (title.starts_with("pay ") && title.contains("{life}"))) && safe_life_payment));
                // Optional exile-and-return: accept only when its chosen target gains
                // from re-entry (an entry ability, or escaping a threat on the stack).
                let useful_reuse = title.contains("you may exile") && title.contains("return") && title.contains("battlefield") && {
                    let staged = self.view.as_ref().and_then(|view| view.stack.iter().rev()
                        .find(|item| item.source_id == prompt_source_id)
                        .and_then(|item| item.targets.iter().find(|target| matches!(target.kind, TargetKind::Card)))
                        .map(|target| target.id.clone()));
                    let remembered = self.pending_reuse_target.as_ref()
                        .filter(|(source, _)| source == &prompt_source_id).map(|(_, target)| target.clone());
                    staged.or(remembered).and_then(|id| self.card(&id))
                        .and_then(|card| self.reentry_value(card, &deciding_player_id))
                        .is_some_and(|value| value > 0)
                };
                // "That player may pay {1}{B}. If the player does, untap that creature":
                // pay on the player's own turn when the enchanted creature is tapped and
                // the cost is affordable.
                let useful_untap = title.contains("may pay") && title.contains("untap that creature")
                    && self.view.as_ref().is_some_and(|view| view.active_player_id == deciding_player_id)
                    && self.card(&prompt_source_id).and_then(|source| source.attached_to.clone())
                        .and_then(|id| self.card(&id))
                        .is_some_and(|creature| creature.tapped && creature.controller_id == deciding_player_id)
                    && presentation.title.split("may pay ").nth(1).and_then(|rest| rest.split('.').next())
                        .is_some_and(|cost| self.cost_affordable(cost, &deciding_player_id));
                // Returning the player's own card from their graveyard, at no other cost.
                let useful_recursion = title.contains("you may return target") && title.contains("from your graveyard")
                    && !["pay", "sacrifice", "discard", "exile", "lose"].iter().any(|word| title.contains(word));
                if useful_reuse || title.contains("you may exile") {
                    self.pending_reuse_target = None;
                }
                let optional_gain = !repeated && self.accepts_optional_effect(&title, &prompt_source_id, &deciding_player_id);
                let value = always_accept || optional_gain || mana_cost_confirmation || useful_single_draw || useful_hand_refill || refill_cost || combo_cost || safe_copy_kicker || pay_single_draw_tax || (accept_once && !repeated)
                    || ((useful_reuse || useful_untap || useful_recursion) && !repeated);
                if useful_hand_refill && prompt_source_id != "none" {
                    self.pending_hand_refill_source = Some(prompt_source_id.clone());
                }
                Some(PromptOutput::ChooseBoolean(ChooseBooleanOutput::Decision { value }))
            }
            PromptInput::ChooseFromSelection(manabrew_protocol::prompts::choose_from_selection::ChooseFromSelectionInput {
                presentation,
                options,
                min_total,
                max_total,
            }) => {
                // These tribal payoffs care about the controller's actual
                // creatures and prospective spells, not the first legal type.
                let tribal_source = matches!(prompt_source_name.as_str(), "Roaming Throne" | "Herald's Horn");
                if tribal_source && presentation.title == "Choose a Creature type" {
                    if let Some(view) = self.view.as_ref() {
                        let best = options.iter().enumerate().filter_map(|(index, option)| {
                            let score: usize = view.zones.iter().filter(|zone|
                                zone.owner_id == deciding_player_id && matches!(zone.zone,
                                    ZoneKind::Battlefield | ZoneKind::Hand | ZoneKind::Command))
                                .map(|zone| zone.cards.iter().map(|card| match card {
                                    CardView::Visible(card) if card.id != prompt_source_id
                                        && card.types.iter().any(|t| t == "Creature")
                                        && card.subtypes.iter().any(|t| t.eq_ignore_ascii_case(&option.label)) => {
                                            let base = if zone.zone == ZoneKind::Command { 4 } else { 2 };
                                            let mentions_type = card.text.split(|ch: char| !ch.is_alphanumeric() && ch != '-')
                                                .any(|word| word.eq_ignore_ascii_case(&option.label)
                                                    || word.strip_suffix('s').is_some_and(|singular| singular.eq_ignore_ascii_case(&option.label)));
                                            base + if mentions_type { 3 } else { 0 }
                                        }
                                    _ => 0,
                                }).sum::<usize>()).sum();
                            (score > 0).then_some((index, score))
                        }).max_by_key(|&(_, score)| score);
                        if let Some((index, _)) = best {
                            return Some(PromptOutput::ChooseFromSelection(ChooseFromSelectionOutput::SelectionDecision {
                                chosen_indices: vec![index],
                            }));
                        }
                    }
                }
                let signature =
                    format!("select:{}|{min_total}|{max_total}|{}", presentation.title, options.len());
                let search = presentation.title.to_ascii_lowercase().contains("search");
                let own_activation = self
                    .attempted_actions
                    .iter()
                    .any(|key| key.starts_with(&format!("ability:{prompt_source_id}:")));
                let useful_extra_recovery_mode = presentation.title.eq_ignore_ascii_case("Choose Mode") && max_total >= 2
                    && options.iter().any(|option| option.label.to_ascii_lowercase().contains("return target creature card from your graveyard to the battlefield"))
                    && self.view.as_ref().is_some_and(|view| view.zones.iter().any(|zone| zone.zone==ZoneKind::Graveyard && zone.owner_id==deciding_player_id && zone.cards.iter().any(|card| matches!(card, CardView::Visible(card) if card.types.iter().any(|kind|kind=="Creature")))));
                let safe_will_modes = prompt_source_name == "Akroma's Will" && presentation.title == "Choose Mode"
                    && max_total >= 2 && options.len() == 2
                    && self.battlefield(&deciding_player_id).any(|card| card.types.iter().any(|t| t == "Creature"))
                    && !self.battlefield(&deciding_player_id).any(|card| card.types.iter().any(|t| t == "Creature") && !card.attachment_ids.is_empty());
                let target = if safe_will_modes || useful_extra_recovery_mode || search || self.looping_on(signature) {
                    max_total
                } else if own_activation {
                    min_total.max(1).min(max_total)
                } else {
                    min_total
                };
                let counterable = self.view.as_ref().is_some_and(|view| {
                    view.stack
                        .iter()
                        .any(|item| item.controller_id != deciding_player_id)
                });
                let mut order = (0..options.len())
                    .filter(|index| {
                        counterable
                            || !options[*index]
                                .label
                                .to_ascii_lowercase()
                                .contains("counter target")
                    })
                    .collect::<Vec<_>>();
                order.sort_by_key(|index| {
                    options[*index]
                        .label
                        .split(':')
                        .next()
                        .map_or(0, |cost| cost.matches('{').count())
                });
                let mut chosen_indices = Vec::new();
                let mut total = 0;
                while total < target {
                    let before = total;
                    for index in order.iter().copied() {
                        let option = &options[index];
                        if total + option.weight > target {
                            continue;
                        }
                        if !option.can_repeat && chosen_indices.contains(&index) {
                            continue;
                        }
                        chosen_indices.push(index);
                        total += option.weight;
                        if total == target {
                            break;
                        }
                    }
                    if total == before {
                        break;
                    }
                }
                Some(PromptOutput::ChooseFromSelection(ChooseFromSelectionOutput::SelectionDecision {
                    chosen_indices,
                }))
            }
            PromptInput::ChooseColor(manabrew_protocol::prompts::choose_color::ChooseColorInput { valid_colors, amount, repeat_allowed, .. }) => {
                let mut chosen: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
                let preferred = if self.awaiting_mana_color && amount == 1 {
                    use manabrew_protocol::game::ManaColor;
                    let name = match self.pending_mana_color {
                        Some(ManaColor::White) => Some("White"), Some(ManaColor::Blue) => Some("Blue"),
                        Some(ManaColor::Black) => Some("Black"), Some(ManaColor::Red) => Some("Red"),
                        Some(ManaColor::Green) => Some("Green"), Some(ManaColor::Colorless) => Some("Colorless"), None => None,
                    };
                    name.and_then(|n| valid_colors.iter().find(|c| c.eq_ignore_ascii_case(n)))
                } else { None };
                self.awaiting_mana_color = false;
                if let Some(c) = preferred {
                    chosen.insert(c.clone(), 1);
                } else if repeat_allowed {
                    if let Some(c) = valid_colors.first() {
                        chosen.insert(c.clone(), amount);
                    }
                } else {
                    for c in valid_colors.iter().take(amount as usize) {
                        chosen.insert(c.clone(), 1);
                    }
                }
                Some(PromptOutput::ChooseColor(ChooseColorOutput::ColorDecision {
                    chosen_colors: chosen,
                }))
            }
            PromptInput::ChooseNumber(manabrew_protocol::prompts::choose_number::ChooseNumberInput { presentation, min, max }) => {
                let x_cost = presentation.title.to_ascii_lowercase().ends_with("for x");
                let life_round = prompt.source_card.as_ref()
                    .filter(|card| card.text.to_ascii_lowercase().contains("pay any amount of life"))
                    .map(|card| card.id.clone());
                let chosen = if x_cost {
                    let fixed = prompt.source_card.as_ref().map_or(0, |card| card.cmc);
                    (self.available_mana(&deciding_player_id) - fixed).clamp(min.max(1).min(max), max)
                } else if let Some(source) = life_round {
                    // Repeats until no one pays: pay once, keeping a 20-life cushion.
                    if self.paid_life_round.as_ref() == Some(&source) {
                        min
                    } else {
                        self.paid_life_round = Some(source);
                        let life = self.view.as_ref()
                            .and_then(|v| v.players.iter().find(|p| p.id == deciding_player_id))
                            .map_or(0, |p| p.life);
                        ((life - 20) / 2).clamp(min, max)
                    }
                } else {
                    min.max(1).min(max)
                };
                Some(PromptOutput::ChooseNumber(ChooseNumberOutput::NumberDecision {
                    chosen_number: Some(chosen),
                }))
            }
            PromptInput::ChooseDamageAssignmentOrder(manabrew_protocol::prompts::choose_damage_assignment_order::ChooseDamageAssignmentOrderInput { mut blocker_ids, .. }) => {
                blocker_ids.sort_by_key(|blocker_id| {
                    self.card(blocker_id)
                        .and_then(|card| card.toughness.as_deref())
                        .and_then(|value| value.parse::<i32>().ok())
                        .unwrap_or(i32::MAX)
                });
                Some(PromptOutput::ChooseDamageAssignmentOrder(ChooseDamageAssignmentOrderOutput::DamageAssignmentOrderDecision {
                    ordered_blocker_ids: blocker_ids,
                }))
            }
            PromptInput::ChooseCombatDamageAssignment(manabrew_protocol::prompts::choose_combat_damage_assignment::ChooseCombatDamageAssignmentInput {
                blocker_ids,
                defender_id,
                total_damage,
                attacker_has_deathtouch,
                ..
            }) => {
                let mut remaining = total_damage.max(0);
                let mut assignments = Vec::new();
                for blocker_id in &blocker_ids {
                    if remaining == 0 {
                        break;
                    }
                    let lethal = if attacker_has_deathtouch {
                        1
                    } else {
                        self.card(blocker_id)
                            .and_then(|card| card.toughness.as_deref().map(|value| (value, card.damage)))
                            .and_then(|(value, damage)| value.parse::<i32>().ok().map(|toughness| toughness - damage))
                            .unwrap_or(remaining)
                            .max(1)
                    };
                    let damage = remaining.min(lethal);
                    remaining -= damage;
                    assignments.push(CombatDamageAssignmentEntry {
                        assignee_id: blocker_id.clone(),
                        damage,
                    });
                }
                if remaining > 0 {
                    if let Some(defender_id) = defender_id {
                        assignments.push(CombatDamageAssignmentEntry {
                            assignee_id: defender_id,
                            damage: remaining,
                        });
                    } else if let Some(last) = assignments.last_mut() {
                        last.damage += remaining;
                    }
                }
                Some(PromptOutput::ChooseCombatDamageAssignment(ChooseCombatDamageAssignmentOutput::CombatDamageAssignmentDecision { assignments }))
            }
            PromptInput::PayManaCost(input) => {
                self.pending_voluntary_cast = None;
                let pool = self.view.as_ref().and_then(|view| {
                    view.players.iter().find(|player| player.id == deciding_player_id)
                        .map(|player| &player.mana_pool)
                });
                use manabrew_protocol::game::ManaColor;
                let colors = [("W", ManaColor::White), ("U", ManaColor::Blue), ("B", ManaColor::Black),
                    ("R", ManaColor::Red), ("G", ManaColor::Green), ("C", ManaColor::Colorless)];
                let mut produced_by_source: std::collections::BTreeMap<&str, std::collections::BTreeSet<ManaColor>> = Default::default();
                for action in &input.actions {
                    if let manabrew_protocol::prompts::common::PaymentActionKind::ActivateManaAbility(info) = &action.kind {
                        let made = produced_by_source.entry(info.card_id.as_str()).or_default();
                        for mana in info.produced_mana.iter().flatten().filter(|m| m.amount > 0) { made.insert(mana.color); }
                    }
                }
                let deficits: Vec<_> = colors.iter().map(|(symbol, color)| {
                    let required = input.mana_cost.split('{').skip(1)
                        .filter(|part| part.split('}').next() == Some(*symbol)).count() as i32;
                    let floating = pool.and_then(|p| p.get(color)).copied().unwrap_or(0) as i32;
                    // Only sources that make nothing else can be counted on for this
                    // color; a flexible source (Mana Confluence, a filter land) must go to
                    // the color the fixed sources cannot cover, or the last shard is
                    // unpayable. A source is judged by every color its actions make.
                    let fixed_sources = produced_by_source.values()
                        .filter(|made| made.len() == 1 && made.contains(color)).count() as i32;
                    // Between colors equally short, the one fewer sources can make comes first.
                    let capable = produced_by_source.values().filter(|made| made.contains(color)).count() as i32;
                    (symbol, color, required, floating, floating + fixed_sources, capable)
                }).filter_map(|(_, color, required, floating, source_count, capable)| {
                    (required > floating).then_some((*color, (required - source_count).max(0), std::cmp::Reverse(capable), required - floating))
                }).collect();
                self.pending_mana_color = deficits.into_iter().max_by_key(|(_, scarce, capable, unpaid)| (*scarce, *capable, *unpaid)).map(|(color, _, _, _)| color);
                self.awaiting_mana_color = false;
                let state = serde_json::to_string(&(&input, pool)).unwrap();
                if self.payment_state.as_deref() != Some(state.as_str()) {
                    self.payment_actions.clear();
                    self.payment_state = Some(state);
                }
                // A costed mana ability can recursively ask for the same
                // generic mana being paid. If a mana ability with no mana
                // cost is already available, let the engine's normal payment
                // path use it instead of manually choosing a costed source.
                let has_mana_ability_without_mana_cost = input.actions.iter().any(|action| {
                    matches!(
                        &action.kind,
                        manabrew_protocol::prompts::common::PaymentActionKind::ActivateManaAbility(info)
                            if !info.cost.as_deref().is_some_and(|cost| {
                                cost.split(", ").any(|part| {
                                    part.split('{').skip(1).any(|symbol| {
                                        symbol.split_once('}').is_some_and(|(value, _)| {
                                            !value.is_empty()
                                                && value.chars().all(|c| c.is_ascii_digit())
                                        })
                                    })
                                })
                            })
                    )
                });
                let cost_symbols = input.mana_cost.split('{').skip(1)
                    .map(|part| part.split('}').next().unwrap_or(""))
                    .map(|s| s.parse::<i32>().ok().or_else(|| (s == "X").then_some(0)).or_else(|| matches!(s, "W" | "U" | "B" | "R" | "G" | "C").then_some(1)))
                    .collect::<Option<Vec<_>>>();
                let need_convoke = cost_symbols.is_some_and(|symbols| {
                    let floating: i32 = pool.map_or(0, |p| p.values().map(|&n| n as i32).sum());
                    symbols.iter().sum::<i32>() > floating + self.available_mana(&deciding_player_id)
                });
                let manual_action = input.actions.iter().filter(|action| {
                    if self.payment_actions.contains(&action.id) {
                        return false;
                    }
                    if has_mana_ability_without_mana_cost {
                        if let manabrew_protocol::prompts::common::PaymentActionKind::ActivateManaAbility(info) = &action.kind {
                            let has_generic_cost = info.cost.as_deref().is_some_and(|cost| cost.split('{').skip(1).any(|part| {
                                part.split_once('}').is_some_and(|(symbol, _)| !symbol.is_empty() && symbol.chars().all(|c| c.is_ascii_digit()))
                            }));
                            // Preserve a filter that supplies a missing color:
                            // the automatic payer cannot activate costed sources.
                            let supplies_missing_color = self.pending_mana_color.is_some_and(|needed| {
                                let this_produces = info.produced_mana.as_ref().is_some_and(|produced| produced.iter().any(|m| m.color == needed && m.amount > 0));
                                let free_produces = input.actions.iter().any(|other| {
                                    let manabrew_protocol::prompts::common::PaymentActionKind::ActivateManaAbility(free) = &other.kind else { return false };
                                    let costed = free.cost.as_deref().is_some_and(|cost| cost.split('{').skip(1).any(|part| {
                                        part.split_once('}').is_some_and(|(symbol, _)| !symbol.is_empty() && symbol.chars().all(|c| c.is_ascii_digit()))
                                    }));
                                    !costed && free.produced_mana.as_ref().is_some_and(|produced| produced.iter().any(|m| m.color == needed && m.amount > 0))
                                });
                                this_produces && !free_produces
                            });
                            if has_generic_cost && !supplies_missing_color { return false; }
                        }
                    }
                    matches!(
                        &action.kind,
                        manabrew_protocol::prompts::common::PaymentActionKind::ActivateManaAbility(
                            info
                        ) if info
                            .cost
                            .as_deref()
                            .is_some_and(|cost| {
                                cost.split(", ").any(|part| {
                                    part.strip_prefix("Pay ")
                                        .and_then(|value| value.strip_suffix(" life"))
                                        .is_some_and(|value| value.parse::<u32>().is_ok())
                                }) || cost.split('{').skip(1).any(|part| {
                                    part.split_once('}').is_some_and(|(symbol, _)| {
                                        !symbol.is_empty()
                                            && symbol.chars().all(|c| c.is_ascii_digit())
                                    })
                                })
                            })
                    ) || matches!(
                        &action.kind,
                        manabrew_protocol::prompts::common::PaymentActionKind::UseResource {
                            resource:
                                manabrew_protocol::prompts::common::PaymentResourceKind::Waterbend,
                            ..
                        }
                    ) || (need_convoke && matches!(
                        &action.kind,
                        manabrew_protocol::prompts::common::PaymentActionKind::UseResource {
                            resource: manabrew_protocol::prompts::common::PaymentResourceKind::Convoke,
                            ..
                        }
                    ))
                }).rev().max_by_key(|action| {
                    if let manabrew_protocol::prompts::common::PaymentActionKind::ActivateManaAbility(info) = &action.kind {
                        if let (Some(needed), Some(produced)) = (self.pending_mana_color, info.produced_mana.as_ref()) {
                            return if produced.iter().any(|m| m.color == needed && m.amount > 0) { 2 } else { 0 };
                        }
                    }
                    1
                });
                let floating: Vec<_> = colors.iter().filter_map(|(_, color)| {
                    pool.and_then(|p| p.get(color)).map(|&n| (*color, n as i32))
                }).collect();
                let payment = if input.can_confirm_from_pool {
                    self.failed_attack_targets.clear();
                    PayManaCostOutput::Pay { auto: false }
                } else if let Some(action) = manual_action {
                    self.payment_actions.insert(action.id.clone());
                    self.selected_mana_source = action.id.strip_prefix("tap:").and_then(|s| s.split(':').next()).map(str::to_owned);
                    self.awaiting_mana_color = matches!(action.kind, manabrew_protocol::prompts::common::PaymentActionKind::ActivateManaAbility(_));
                    PayManaCostOutput::Act {
                        action_id: action.id.clone(),
                    }
                } else if let Some(action_id) = payment::planned_tap(&input.mana_cost, &floating, &input.actions, &self.payment_actions) {
                    // Pay the coloured pips ourselves when a source with a choice of colour could be
                    // wasted by the automatic payer (see `payment`).
                    self.payment_actions.insert(action_id.clone());
                    self.selected_mana_source = action_id.strip_prefix("tap:").and_then(|s| s.split(':').next()).map(str::to_owned);
                    self.awaiting_mana_color = false;
                    PayManaCostOutput::Act { action_id }
                } else if input.actions.is_empty()
                    || self.payment_attempt.as_deref() == Some(input.card_id.as_str())
                {
                    let phyrexian_life = input.mana_cost.split('{').skip(1)
                        .filter(|part| part.split('}').next().is_some_and(|symbol| symbol.ends_with("/P")))
                        .count() as i32 * 2;
                    let life = self.view.as_ref().and_then(|view| view.players.iter().find(|p| p.id == deciding_player_id))
                        .map_or(0, |p| p.life);
                    if input.actions.is_empty() && phyrexian_life > 0 && life > phyrexian_life
                        && self.payment_attempt.as_deref() != Some(input.card_id.as_str()) {
                        self.payment_attempt = Some(input.card_id.clone());
                        PayManaCostOutput::Pay { auto: true }
                    } else {
                        self.fail_attack_target(&input.card_id);
                        self.payment_attempt = None;
                        PayManaCostOutput::Cancel
                    }
                } else {
                    self.payment_attempt = Some(input.card_id.clone());
                    PayManaCostOutput::Pay { auto: true }
                };
                self.auto_payment_active = matches!(payment, PayManaCostOutput::Pay { auto: true });
                if !matches!(payment, PayManaCostOutput::Act { .. }) { self.selected_mana_source = None; }
                Some(PromptOutput::PayManaCost(payment))
            }
            PromptInput::ChooseCards(manabrew_protocol::prompts::choose_cards::ChooseCardsInput {
                presentation,
                mut cards,
                min,
                max,
            }) => {
                let title = presentation.title.to_ascii_lowercase();
                if title.starts_with("separate the cards into two piles") {
                    return Some(PromptOutput::ChooseCards(ChooseCardsOutput::ChooseCardsDecision {
                        chosen_card_ids: Self::pile_one_cards(&cards, min, max),
                    }));
                }
                if let Some(split) = title.strip_prefix("choose a pile: the first ").and_then(|rest| rest.split_whitespace().next()).and_then(|n| n.parse::<usize>().ok()) {
                    return Some(PromptOutput::ChooseCards(ChooseCardsOutput::ChooseCardsDecision {
                        chosen_card_ids: Self::better_pile_card(&cards, split).into_iter().collect(),
                    }));
                }
                let discard = title.contains("discard");
                let hand_reorder = prompt_source_text.contains("from your hand on top")
                    || prompt_source_text.contains("from your hand on the bottom");
                let prefer_low = title.contains("sacrifice")
                    || discard
                    || title.contains("graveyard")
                    || title.contains("bottom")
                    || title.contains("kor skyfisher")
                    || title.contains("glint hawk")
                    || hand_reorder;
                let lands = self.lands_in_play(&deciding_player_id);
                let missing = self.missing_colors(&deciding_player_id);
                let recover_tutor = title.contains("zone change") && self.view.as_ref().is_some_and(|view| {
                    view.zones.iter().any(|zone|matches!(zone.zone, ZoneKind::Graveyard | ZoneKind::Battlefield) && zone.owner_id==deciding_player_id && zone.cards.iter().any(|card|matches!(card, CardView::Visible(card) if {
                        let text=card.text.to_ascii_lowercase();text.contains("dies") && text.contains("search your library") && text.contains("creature cards")
                    })))
                });
                cards.sort_by_key(|card| {
                    let land = card.types.iter().any(|ty| ty == "Land");
                    let value = if land {
                        let fixes = Self::land_colors(card)
                            .iter()
                            .filter(|color| missing.contains(color))
                            .count() as i32;
                        if lands < 6 { 40 + fixes * 10 } else { fixes * 10 }
                    } else if prefer_low {
                        Self::card_value(card) - (card.cmc - lands as i32 - 3).max(0) * 10
                    } else {
                        Self::card_value(card)
                    };
                    let tutor_on_death = title.contains("sacrifice") && {
                        let text=card.text.to_ascii_lowercase();
                        text.contains("dies") && text.contains("search your library") && text.contains("creature cards") && text.contains("onto the battlefield")
                    };
                    let revives_tutor = recover_tutor && card.text.to_ascii_lowercase().contains("return target creature card from your graveyard to the battlefield");
                    if tutor_on_death || revives_tutor { -10_000 } else if prefer_low { value } else { -value }
                });
                let count = if (discard || hand_reorder) && min == 0 {
                    max
                } else if prefer_low {
                    min
                } else {
                    max
                };
                let chosen_card_ids: Vec<_> = cards.iter().take(count).map(|card| card.id.clone()).collect();
                if recover_tutor {
                    self.known_tutor_targets=Some(cards.iter().filter(|card|!chosen_card_ids.contains(&card.id)).map(|card|card.id.clone()).collect());
                }
                Some(PromptOutput::ChooseCards(ChooseCardsOutput::ChooseCardsDecision { chosen_card_ids }))
            }
            PromptInput::Reorder(manabrew_protocol::prompts::reorder::ReorderInput { items, .. }) => {
                Some(PromptOutput::Reorder(ReorderOutput::ReorderDecision {
                    ordered_ids: items.iter().map(|item| item.id.clone()).collect(),
                }))
            }
            PromptInput::GameOver(manabrew_protocol::prompts::game_over::GameOverInput { .. }) => None,
            // Display-only acknowledgements: the engine `await`s these so
            // every transport must produce an ack — keeps the engine's
            // broadcast loop polymorphic (no `if is_human` branching).
            PromptInput::DiceRolled(_) => Some(PromptOutput::DiceRolled(
                DiceRolledOutput::DiceRolledAcknowledged,
            )),
        }
    }
}

#[cfg(test)]
mod required_attack_regression {
    use crate::agent::BotAgent;
    use super::SimpleAi;
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;

    #[test]
    fn required_attacker_is_not_dropped_on_repeated_prompt() {
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId":1,"decidingPlayerId":"player-0",
            "input":{"type":"chooseAttackers", "attackers":[
                {"attackerId":"card-1", "mustAttack":true, "validTargetIds":["player-1"]}],
                "attackTargets":[{"id":"player-1", "kind":"player", "label":"Player 1"}]}
        })).unwrap();
        let mut bot = SimpleAi::default();
        for _ in 0..2 {
            let response = serde_json::to_value(bot.decide(prompt.clone()).unwrap()).unwrap();
            assert_eq!(response["output"]["assignments"][0]["attackerId"], "card-1", "{response}");
        }
    }
    #[test]
    fn global_limit_chooses_one_of_two_required_attackers() {
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId":1,"decidingPlayerId":"player-0",
            "input":{"type":"chooseAttackers", "maxAttackers":1, "attackers":[
                {"attackerId":"card-1", "mustAttack":true, "validTargetIds":["player-1"]},
                {"attackerId":"card-2", "mustAttack":true, "validTargetIds":["player-1"]}],
                "attackTargets":[{"id":"player-1", "kind":"player", "label":"Player 1"}]}
        })).unwrap();
        let mut bot = SimpleAi::default();
        for _ in 0..2 {
            let response = serde_json::to_value(bot.decide(prompt.clone()).unwrap()).unwrap();
            assert_eq!(response["output"]["assignments"].as_array().unwrap().len(), 1);
        }
    }
    #[test]
    fn two_required_attackers_respect_per_defender_limit() {
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId":1,"decidingPlayerId":"player-0",
            "input":{"type":"chooseAttackers", "targetLimits":{"player-2":1}, "attackers":[
                {"attackerId":"card-1", "mustAttack":true, "validTargetIds":["player-1","player-2"]},
                {"attackerId":"card-2", "mustAttack":true, "validTargetIds":["player-1","player-2"]}],
                "attackTargets":[{"id":"player-1", "kind":"player", "label":"Player 1"},
                                 {"id":"player-2", "kind":"player", "label":"Player 2"}]}
        })).unwrap();
        let response = serde_json::to_value(SimpleAi::default().decide(prompt).unwrap()).unwrap();
        let choices = response["output"]["assignments"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert!(choices.iter().filter(|choice| choice["targetId"] == "player-2").count() <= 1);
    }
    #[test]
    fn constrained_required_attacker_gets_limited_defender_first() {
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId":1,"decidingPlayerId":"player-0", "input":{"type":"chooseAttackers",
                "targetLimits":{"player-2":1}, "attackers":[
                    {"attackerId":"card-1", "mustAttack":true, "validTargetIds":["player-1","player-2"]},
                    {"attackerId":"card-2", "mustAttack":true, "validTargetIds":["player-2"]}],
                "attackTargets":[{"id":"player-1", "kind":"player", "label":"Player 1"},
                                 {"id":"player-2", "kind":"player", "label":"Player 2"}]}})).unwrap();
        let response = serde_json::to_value(SimpleAi::default().decide(prompt).unwrap()).unwrap();
        let choices = response["output"]["assignments"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0]["attackerId"], "card-2");
        assert_ne!(choices[0]["targetId"], choices[1]["targetId"]);
    }
    #[test]
    fn goaded_attacker_uses_goaders_target_only_when_alternative_full() {
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId":1,"decidingPlayerId":"player-0", "input":{"type":"chooseAttackers",
                "targetLimits":{"player-2":1}, "attackers":[
                    {"attackerId":"card-1", "mustAttack":true, "goadedBy":"player-1", "validTargetIds":["player-1","player-2"]},
                    {"attackerId":"card-2", "mustAttack":true, "goadedBy":"player-1", "validTargetIds":["player-1","player-2"]}],
                "attackTargets":[{"id":"player-1", "kind":"player", "label":"Player 1"},
                                 {"id":"player-2", "kind":"player", "label":"Player 2"}]}})).unwrap();
        let response = serde_json::to_value(SimpleAi::default().decide(prompt).unwrap()).unwrap();
        let choices = response["output"]["assignments"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        let mut targets: Vec<_> = choices.iter().map(|choice| choice["targetId"].as_str().unwrap()).collect();
        targets.sort();
        assert_eq!(targets, vec!["player-1", "player-2"]);
    }
    #[test]
    fn goad_keeps_other_player_targets_when_capacity_allows() {
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId":1,"decidingPlayerId":"player-0", "input":{"type":"chooseAttackers",
                "targetLimits":{"player-2":1,"player-3":1}, "attackers":[
                    {"attackerId":"card-1", "mustAttack":true, "goadedBy":"player-1", "validTargetIds":["player-2","player-1"]},
                    {"attackerId":"card-2", "mustAttack":true, "goadedBy":"player-1", "validTargetIds":["player-2","player-3","player-1"]}],
                "attackTargets":[{"id":"player-1", "kind":"player", "label":"Player 1"},
                    {"id":"player-2", "kind":"player", "label":"Player 2"},
                    {"id":"player-3", "kind":"player", "label":"Player 3"}]}})).unwrap();
        let response = serde_json::to_value(SimpleAi::default().decide(prompt).unwrap()).unwrap();
        let choices = response["output"]["assignments"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert!(choices.iter().all(|choice| choice["targetId"] != "player-1"));
    }
    #[test]
    fn required_attacks_reassign_flexible_targets_when_capacities_compete() {
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId":1,"decidingPlayerId":"player-0", "input":{"type":"chooseAttackers",
                "targetLimits":{"player-1":1,"player-2":1,"player-3":1,"player-4":1}, "attackers":[
                    {"attackerId":"card-1", "mustAttack":true, "validTargetIds":["player-1","player-2"]},
                    {"attackerId":"card-2", "mustAttack":true, "validTargetIds":["player-2","player-3"]},
                    {"attackerId":"card-3", "mustAttack":true, "validTargetIds":["player-2","player-3"]},
                    {"attackerId":"card-4", "mustAttack":true, "validTargetIds":["player-1","player-4"]}],
                "attackTargets":[{"id":"player-1", "kind":"player", "label":"Player 1"},
                    {"id":"player-2", "kind":"player", "label":"Player 2"},
                    {"id":"player-3", "kind":"player", "label":"Player 3"},
                    {"id":"player-4", "kind":"player", "label":"Player 4"}]}})).unwrap();
        let response = serde_json::to_value(SimpleAi::default().decide(prompt).unwrap()).unwrap();
        assert_eq!(response["output"]["assignments"].as_array().unwrap().len(), 4);
    }
    #[test]
    fn required_attacker_with_no_legal_target_is_not_assigned_an_invalid_one() {
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId":1,"decidingPlayerId":"player-0",
            "input":{"type":"chooseAttackers", "attackers":[
                {"attackerId":"card-1", "mustAttack":true, "validTargetIds":[]}],
                "attackTargets":[{"id":"player-1", "kind":"player", "label":"Player 1"}]}
        })).unwrap();
        let response = serde_json::to_value(SimpleAi::default().decide(prompt).unwrap()).unwrap();
        assert_eq!(response["output"]["assignments"], json!([]));
    }
}

#[cfg(test)]
mod optional_draw_regression {
    use crate::agent::BotAgent;
    use super::SimpleAi;
    use manabrew_protocol::{game::{GameViewDto, ZoneDto, ZoneKind}, transport::AgentPrompt};
    use serde_json::json;

    #[test]
    fn accepts_useful_optional_draw_but_declines_empty_library() {
        for (count, title, expected) in [(2, "Do you want to draw 1 card(s)?", true), (0, "Do you want to draw 1 card(s)?", false), (2, "Do you want to draw 2 card(s)?", false), (2, "Do you want to draw a card?", true), (2, "Whenever enchanted creature deals damage to an opponent, you may draw a card.", true), (0, "Whenever enchanted creature deals damage to an opponent, you may draw a card.", false), (2, "Whenever this attacks, you may draw a card, then discard a card.", false)] {
            let mut bot = SimpleAi::default();
            let mut view = GameViewDto::default();
            view.zones.push(ZoneDto { zone: ZoneKind::Library, owner_id: "player-0".into(), cards: vec![], count });
            bot.observe(view);
            let prompt: AgentPrompt = serde_json::from_value(json!({
                "promptId":1,"decidingPlayerId":"player-0",
                "input":{"type":"chooseBoolean","confirmLabel":"Accept","denyLabel":"Decline",
                    "presentation":{"title":title,"targets":[]}}
            })).unwrap();
            let response = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();
            assert_eq!(response["output"]["value"], expected);
        }
    }
}

#[cfg(test)]
mod optional_tax_regression {
    use crate::agent::BotAgent;
    use super::SimpleAi;
    use manabrew_protocol::{game::{CardDto, CardView, GameViewDto, ZoneDto, ZoneKind}, transport::AgentPrompt};
    use serde_json::json;

    #[test]
    fn tax_policy_pays_with_spare_land_and_declines_last_source() {
        for (lands, expected) in [(2, true), (1, false)] {
            let mut bot = SimpleAi::default();
            let mut view = GameViewDto::default();
            view.zones.push(ZoneDto { zone: ZoneKind::Battlefield, owner_id: "player-0".into(), count: lands,
                cards: (0..lands).map(|i| CardView::Visible(CardDto { id: format!("card-{i}"),
                    controller_id: "player-0".into(), types: vec!["Land".into()], ..Default::default() })).collect() });
            bot.observe(view);
            let prompt: AgentPrompt = serde_json::from_value(json!({
                "promptId":1,"decidingPlayerId":"player-0",
                "input":{"type":"chooseBoolean","confirmLabel":"Pay","denyLabel":"Decline",
                    "presentation":{"title":"Pay {1} to prevent an opponent drawing a card?","targets":[]}}
            })).unwrap();
            let response = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();
            assert_eq!(response["output"]["value"], expected);
        }
    }
}

#[cfg(test)]
mod convoke_payment_regression {
    use crate::agent::BotAgent;
    use super::SimpleAi;
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;
    #[test]
    fn uses_convoke_when_payment_is_not_ready_and_confirms_when_ready() {
        for ready in [false, true] {
            let prompt: AgentPrompt = serde_json::from_value(json!({
                "promptId":1,"decidingPlayerId":"player-0",
                "input":{"type":"payManaCost", "presentation":{"title":"Pay"}, "cardId":"card-9", "cardName":"Convoke spell", "manaCost":"{1}{W}", "canConfirmFromPool":ready, "actions":[
                    {"id":"convoke:card-1", "type":"useResource", "cardId":"card-1", "resource":"convoke"}]}
            })).unwrap();
            let output=serde_json::to_value(SimpleAi::default().decide(prompt).unwrap()).unwrap();
            if ready { assert_eq!(output["output"]["type"],"pay"); assert_eq!(output["output"]["auto"],false); }
            else { assert_eq!(output["output"]["type"],"act"); assert_eq!(output["output"]["actionId"],"convoke:card-1"); }
        }
    }
}

#[cfg(test)]
mod floating_mana_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{CardDto,CardView,GameViewDto,PlayerDto,ZoneDto,ZoneKind};
    use serde_json::json;
    #[test]
    fn floating_red_plus_wild_source_pays_for_blue_spell_but_red_alone_cannot() {
        for (red, treasure, expected) in [(2,true,true),(2,false,false),(1,true,false),(0,true,false)] {
            let mut bot=SimpleAi::default(); let mut view=GameViewDto::default();
            let p:PlayerDto=serde_json::from_value(json!({"id":"player-0","name":"A","status":"playing","isHuman":false,"life":40,"counters":{},"manaPool":{"R":red},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap();
            view.players.push(p);
            if treasure { view.zones.push(ZoneDto {zone:ZoneKind::Battlefield, owner_id:"player-0".into(),count:1,cards:vec![CardView::Visible(CardDto {id:"card-2".into(),types:vec!["Artifact".into()],text:"{T}, Sacrifice this: Add one mana of any color.".into(),..Default::default()})]}); }
            bot.observe(view);
            let spell=CardDto {mana_cost:"{2}{U}".into(),cmc:3,..Default::default()};
            assert_eq!(bot.affordable(&spell,"player-0"),expected,"red={red}, treasure={treasure}");
        }
    }
}

#[cfg(test)]
mod convoke_affordability_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{CardDto,CardView,GameViewDto,ZoneDto,ZoneKind};
    #[test]
    fn untapped_white_creatures_can_fund_a_convoke_spell_once_each() {
        for (n,expected) in [(3,false),(4,true)] {
            let mut bot=SimpleAi::default(); let mut view=GameViewDto::default();
            view.zones.push(ZoneDto {zone:ZoneKind::Battlefield,owner_id:"player-0".into(),count:n,cards:(0..n).map(|i|CardView::Visible(CardDto {id:format!("card-{i}"),color:"W".into(),types:vec!["Creature".into()],summoning_sick:true,..Default::default()})).collect()});
            bot.observe(view);
            let spell=CardDto {mana_cost:"{2}{W}{W}".into(),cmc:4,keywords:vec!["Convoke".into()],..Default::default()};
            assert_eq!(bot.affordable(&spell,"player-0"),expected);
        }
    }
}

#[cfg(test)]
mod recursive_mana_payment_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;

    fn choice_for(cost: &str, extra_cost: &str) -> serde_json::Value {
        let mut bot = SimpleAi::default();
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId": 1, "decidingPlayerId": "player-0",
            "input": {"type": "payManaCost", "presentation": {"title": "Pay"},
                "cardId": "spell", "cardName": "Spell", "manaCost": cost, "canConfirmFromPool": false,
                "actions": [
                    {"id":"tap:free:0", "type":"activateManaAbility", "cardId":"free", "abilityIndex":0,
                     "description":"Add colorless", "isManaAbility":true, "cost":"{T}",
                     "producedMana":[{"amount":1,"color":"C"}]},
                    {"id":"tap:filter:0:W", "type":"activateManaAbility", "cardId":"filter", "abilityIndex":0,
                     "description":"Add white", "isManaAbility":true, "cost":extra_cost,
                     "producedMana":[{"amount":1,"color":"W"}]}
                ]}
        })).unwrap();
        serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone()
    }

    #[test]
    fn retains_colored_filter_when_free_sources_cannot_supply_the_color() {
        assert_eq!(choice_for("{W}", "{1}, {T}")["actionId"], "tap:filter:0:W");
    }

    #[test]
    fn retains_life_costed_mana_ability() {
        assert_eq!(choice_for("{W}", "{T}, Pay 1 life")["actionId"], "tap:filter:0:W");
    }

    /// DEV-072: Kaalia's {1}{R}{W}{B}: the fixed sources make R and B, so the flexible Mana
    /// Confluence is spent on the W only it can make.
    #[test]
    fn a_flexible_source_goes_to_the_color_no_fixed_source_makes() {
        let tap = |card: &str, index: i32, suffix: &str, cost: &str, color: &str| json!({
            "id": format!("tap:{card}:{index}{suffix}"), "type": "activateManaAbility", "cardId": card, "abilityIndex": index,
            "description": "Add mana", "isManaAbility": true, "cost": cost, "producedMana": [{"amount": 1, "color": color}]});
        let mut actions = vec![tap("card-289", 0, "", "{T}", "B"), tap("card-280", 0, "", "{T}", "C"),
            tap("card-280", 1, ":B", "{T}", "B"), tap("card-280", 1, ":R", "{T}", "R"), tap("card-301", 0, "", "{T}", "R")];
        for color in ["W", "U", "B", "R", "G"] {
            actions.push(tap("card-269", 0, &format!(":{color}"), "{T}, Pay 1 life", color));
        }
        let mut bot = SimpleAi::default();
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId": 1, "decidingPlayerId": "player-0",
            "input": {"type": "payManaCost", "presentation": {"title": "Pay"}, "cardId": "card-202", "cardName": "Kaalia of the Vast",
                "manaCost": "{1}{R}{W}{B}", "canConfirmFromPool": false, "actions": actions}
        })).unwrap();
        let out = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone();
        assert_eq!(out["actionId"], "tap:card-269:0:W");
    }

    /// DEV-072: when R and W are both short of fixed sources, the flexible Confluence goes to W,
    /// the color only it can make, and the filter lands cover R.
    #[test]
    fn the_color_fewer_sources_can_make_is_chosen_first() {
        let tap = |card: &str, index: i32, suffix: &str, cost: &str, color: &str| json!({
            "id": format!("tap:{card}:{index}{suffix}"), "type": "activateManaAbility", "cardId": card, "abilityIndex": index,
            "description": "Add mana", "isManaAbility": true, "cost": cost, "producedMana": [{"amount": 1, "color": color}]});
        let mut actions = vec![tap("card-188", 0, "", "{T}", "B"), tap("card-189", 0, "", "{T}", "B"),
            tap("card-184", 1, ":B", "{T}", "B"), tap("card-184", 1, ":R", "{T}", "R"),
            tap("card-181", 0, ":B", "{T}", "B"), tap("card-181", 0, ":R", "{T}", "R")];
        for color in ["W", "U", "B", "R", "G"] {
            actions.push(tap("card-168", 0, &format!(":{color}"), "{T}, Pay 1 life", color));
        }
        let mut bot = SimpleAi::default();
        let prompt: AgentPrompt = serde_json::from_value(json!({
            "promptId": 1, "decidingPlayerId": "player-0",
            "input": {"type": "payManaCost", "presentation": {"title": "Pay"}, "cardId": "card-101", "cardName": "Kaalia of the Vast",
                "manaCost": "{1}{R}{W}{B}", "canConfirmFromPool": false, "actions": actions}
        })).unwrap();
        let out = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone();
        assert_eq!(out["actionId"], "tap:card-168:0:W");
    }

    #[test]
    fn uses_free_mana_options_instead_of_alternating_costed_one_for_one_sources() {
        let mut bot = SimpleAi::default();
        for (prompt_id, costed_source) in [(1, "card-69"), (2, "card-68")] {
            let prompt: AgentPrompt = serde_json::from_value(json!({
                "promptId": prompt_id,
                "decidingPlayerId": "player-0",
                "input": {
                    "type": "payManaCost",
                    "presentation": {"title": "Pay"},
                    "cardId": costed_source,
                    "cardName": "Filter land",
                    "manaCost": "{1}",
                    "canConfirmFromPool": false,
                    "actions": [
                        {
                            "id": "tap:free-land:0",
                            "type": "activateManaAbility",
                            "cardId": "free-land",
                            "abilityIndex": 0,
                            "description": "Add {C}.",
                            "isManaAbility": true,
                            "cost": "{T}",
                            "producedMana": [{"amount": 1, "color": "C"}]
                        },
                        {
                            "id": format!("tap:{costed_source}:1:W"),
                            "type": "activateManaAbility",
                            "cardId": costed_source,
                            "abilityIndex": 1,
                            "description": "Add one mana of any color.",
                            "isManaAbility": true,
                            "cost": "{1}, {T}",
                            "producedMana": [{"amount": 1, "color": "W"}]
                        }
                    ]
                }
            })).unwrap();
            let response = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();
            assert_eq!(response["output"]["type"], "pay");
            assert_eq!(response["output"]["auto"], true);
        }
    }
}

#[cfg(test)]
mod attack_copy_timing_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{CardDto,CardView,GameViewDto,ZoneDto,ZoneKind,StepKind};
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;
    #[test]
    fn reserve_resource_instant_until_attacking_but_use_it_during_combat() {
        for (step,own_stack,expected) in [(StepKind::Main1,false,"pass"),(StepKind::Main1,true,"pass"),(StepKind::CombatDeclareAttackers,false,"act"),(StepKind::Main2,false,"act")] {
            let mut bot=SimpleAi::default(); let mut view=GameViewDto::default(); view.active_player_id="player-0".into(); view.step=step;
            if own_stack { view.stack.push(manabrew_protocol::game::StackObjectDto {controller_id:"player-0".into(),..Default::default()}); }
            let source=CardDto {id:"card-1".into(), types:vec!["Creature".into()], text:"Whenever you cast a spell while this creature is attacking, copy that spell.".into(),..Default::default()};
            let lands=(0..3).map(|i|CardView::Visible(CardDto {id:format!("card-{}",i+3),types:vec!["Land".into()],subtypes:vec!["Island".into()],..Default::default()}));
            view.zones.push(ZoneDto {zone:ZoneKind::Battlefield,owner_id:"player-0".into(),count:4,cards:std::iter::once(CardView::Visible(source)).chain(lands).collect()});
            let spell=CardDto {id:"card-2".into(),types:vec!["Instant".into()],mana_cost:"{2}{U}".into(),cmc:3,text:"Draw two cards.".into(),..Default::default()};
            view.zones.push(ZoneDto {zone:ZoneKind::Hand,owner_id:"player-0".into(),count:1,cards:vec![CardView::Visible(spell)]}); bot.observe(view);
            let prompt:AgentPrompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","input":{"type":"chooseAction","actions":[{"type":"cast","id":"cast:card-2:normal","cardId":"card-2","mode":{"type":"normal"},"label":"Cast normally"}]}})).unwrap();
            let response=serde_json::to_value(bot.decide(prompt).unwrap()).unwrap(); assert_eq!(response["output"]["type"],expected,"{response}");
        }
    }
}

#[cfg(test)]
mod phasing_timing_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{CardDto,CardView,GameViewDto,ZoneDto,ZoneKind,StepKind,StackObjectDto};
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;
    #[test]
    fn mass_phasing_is_not_spent_on_an_unthreatened_own_main_phase() {
        for (foreign,effect,expected) in [(false,"Destroy all creatures", "pass"),(true,"Draw a card", "pass"),(true,"Destroy all creatures", "act")] {
            let mut bot=SimpleAi::default(); let mut view=GameViewDto::default(); view.active_player_id="player-0".into();view.step=StepKind::Main1;
            view.stack.push(StackObjectDto {controller_id: if foreign {"player-1"} else {"player-0"}.into(),text:effect.into(),..Default::default()});
            view.zones.push(ZoneDto {zone:ZoneKind::Battlefield,owner_id:"player-0".into(),count:4,cards:(0..4).map(|i|CardView::Visible(CardDto {id:format!("card-{}",i+3),types:vec!["Land".into()],subtypes:vec!["Plains".into()],..Default::default()})).collect()});
            view.zones.push(ZoneDto {zone:ZoneKind::Hand,owner_id:"player-0".into(),count:1,cards:vec![CardView::Visible(CardDto {id:"card-2".into(),types:vec!["Instant".into()],mana_cost:"{2}{W}{W}".into(),cmc:4,text:"Any number of target nonland permanents you control phase out.".into(),..Default::default()})]});bot.observe(view);
            let prompt:AgentPrompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","input":{"type":"chooseAction","actions":[{"type":"cast","id":"cast:card-2:normal","cardId":"card-2","mode":{"type":"normal"},"label":"Cast normally"}]}})).unwrap();
            let output=serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();assert_eq!(output["output"]["type"],expected,"{output}");
        }
    }
}

#[cfg(test)]
mod discounted_action_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{CardDto,CardView,GameViewDto,ZoneDto,ZoneKind,StepKind};
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;
    #[test]
    fn offered_discounted_creature_is_not_rejected_by_printed_mana_value() {
        for (reducer,expected) in [(false,"pass"),(true,"act")] {
            let mut bot=SimpleAi::default();let mut view=GameViewDto::default();view.active_player_id="player-0".into();view.step=StepKind::Main1;
            let mut board:Vec<_>=(0..4).map(|i|CardView::Visible(CardDto {id:format!("card-{}",i+3),types:vec!["Land".into()],subtypes:vec!["Plains".into()],..Default::default()})).collect();
            if reducer {board.push(CardView::Visible(CardDto {id:"card-9".into(),types:vec!["Creature".into()],text:"The first creature spell with flying you cast each turn costs {1} less to cast.".into(),..Default::default()}));}
            view.zones.push(ZoneDto {zone:ZoneKind::Battlefield,owner_id:"player-0".into(),count:board.len(),cards:board});
            view.zones.push(ZoneDto {zone:ZoneKind::Hand,owner_id:"player-0".into(),count:1,cards:vec![CardView::Visible(CardDto {id:"card-2".into(),types:vec!["Creature".into()],mana_cost:"{3}{W}{W}".into(),cmc:5,..Default::default()})]});bot.observe(view);
            let prompt:AgentPrompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","input":{"type":"chooseAction","actions":[{"type":"cast","id":"cast:card-2:normal","cardId":"card-2","mode":{"type":"normal"},"label":"Cast normally"}]}})).unwrap();
            let output=serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();assert_eq!(output["output"]["type"],expected,"{output}");
        }
    }
}

#[cfg(test)]
mod hand_refill_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::{game::{GameViewDto, ZoneDto, ZoneKind}, transport::AgentPrompt};
    use serde_json::json;
    fn decide(bot:&mut SimpleAi, source:&str, title:&str, label:&str)->bool {
        let prompt:AgentPrompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0",
            "sourceCard":{"id":source},"input":{"type":"chooseBoolean","confirmLabel":label,"denyLabel":"Decline",
            "presentation":{"title":title,"targets":[]}}})).unwrap();
        serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"]["value"].as_bool().unwrap()
    }
    fn bot(hand:usize, library:usize)->SimpleAi {
        let mut bot=SimpleAi::default();let mut view=GameViewDto::default();
        for (zone,count) in [(ZoneKind::Hand,hand),(ZoneKind::Library,library)] {
            view.zones.push(ZoneDto {zone,owner_id:"player-0".into(),cards:vec![],count});
        }
        bot.observe(view);bot
    }
    #[test]
    fn refills_only_depleted_hands_and_confirms_only_immediate_matching_cost() {
        let title="Whenever the Ring tempts you, you may discard your hand. If you do, draw four cards.";
        for (hand,library,expected) in [(0,8,true),(1,8,true),(2,8,false),(5,8,false),(0,3,false)] {
            let mut bot=bot(hand,library);
            assert_eq!(decide(&mut bot,"card-7",title,"Accept"),expected);
            assert_eq!(decide(&mut bot,"card-7","Pay Cost for Sauron, the Dark Lord?","Pay"),expected);
        }
        let mut other=bot(0,8);
        assert!(decide(&mut other,"card-7",title,"Accept"));
        assert!(!decide(&mut other,"card-8","Pay Cost for something else?","Pay"));
        assert!(!decide(&mut other,"card-7","Pay Cost for Sauron, the Dark Lord?","Pay"));
        let mut interrupted=bot(0,8);
        assert!(decide(&mut interrupted,"card-7",title,"Accept"));
        assert!(!decide(&mut interrupted,"card-7","Unrelated choice?","Accept"));
        assert!(!decide(&mut interrupted,"card-7","Pay Cost for Sauron, the Dark Lord?","Pay"));
        let mut unrelated=bot(0,8);
        assert!(!decide(&mut unrelated,"card-7","Pay Cost for Sauron, the Dark Lord?","Pay"));
    }
}

#[cfg(test)]
mod voluntary_answer_scope_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{GameViewDto, StackObjectDto};
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;

    #[test]
    fn voluntary_redirection_can_target_a_friendly_spell() {
        let mut bot=SimpleAi::default(); let mut view=GameViewDto::default();
        view.stack.push(StackObjectDto { id:"stack-1".into(),controller_id:"player-0".into(),..Default::default() });
        bot.observe(view); bot.pending_voluntary_cast=Some("card-2".into());
        let prompt:AgentPrompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0",
            "sourceCard":{"id":"card-2","identity":{"name":"Bolt Bend"}},
            "sourceAbilityText":"Change the target of target spell or ability with a single target.",
            "input":{"type":"chooseBoardTargets","presentation":{"title":"Counter","targets":[]},
                "candidates":[{"kind":"spell","id":"stack-1"}],"hostile":true,"intent":"counter",
                "minTargets":1,"maxTargets":1,"chosenTargets":0,"cancellable":true}})).unwrap();
        let output=serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();
        assert_eq!(output["output"]["type"],"boardTargets","{output}");
    }

    #[test]
    fn a_pending_redirect_is_not_a_sufficient_counter() {
        use manabrew_protocol::game::CardDto;
        use manabrew_protocol::prompts::common::TargetRef;
        let target:TargetRef=serde_json::from_value(json!({"id":"stack-1","kind":"spell","intent":"counter"})).unwrap();
        let mut view=GameViewDto::default();
        view.stack.push(StackObjectDto { id:"stack-1".into(),controller_id:"player-1".into(),source_card:Some(CardDto::default()),..Default::default() });
        view.stack.push(StackObjectDto { id:"stack-2".into(),controller_id:"player-0".into(),source_ability_text:Some("Change the target of target spell or ability with a single target.".into()),targets:vec![target.clone()],..Default::default() });
        let mut bot=SimpleAi::default();bot.observe(view);
        assert!(!bot.covered_by_friendly_answer(&target,"player-0"));
    }

    #[test]
    fn mandatory_and_copied_target_prompts_keep_their_only_legal_target() {
        // A copied/triggered effect is not an ordinary hand-cast action.
        // Cancellable is deliberately true in one case to guard that distinction.
        for cancellable in [false, true] {
            let mut bot=SimpleAi::default(); let mut view=GameViewDto::default();
            view.stack.push(StackObjectDto { id:"stack-1".into(),controller_id:"player-0".into(),..Default::default() });
            bot.observe(view);
            let prompt:AgentPrompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0",
                "sourceCard":{"id":"card-2","identity":{"name":"Counterspell"}},
                "input":{"type":"chooseBoardTargets","presentation":{"title":"Counter","targets":[]},
                    "candidates":[{"kind":"spell","id":"stack-1"}],"hostile":true,"intent":"counter",
                    "minTargets":1,"maxTargets":1,"chosenTargets":0,"cancellable":cancellable}})).unwrap();
            let output=serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();
            assert_eq!(output["output"]["type"],"boardTargets","{output}");
            assert_eq!(output["output"]["chosen"][0]["id"],"stack-1","{output}");
        }
    }
}

#[cfg(test)]
mod pending_damage_scope_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{GameViewDto,StackObjectDto,PlayerDto};
    use manabrew_protocol::prompts::common::TargetRef;
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;

    fn player_target(id:&str)->TargetRef {
        serde_json::from_value(json!({"id":id,"kind":"player","intent":"damage"})).unwrap()
    }
    #[test]
    fn reservations_follow_current_stack_targets_and_threats() {
        let mut view=GameViewDto::default();
        view.stack.push(StackObjectDto{id:"stack-1".into(),controller_id:"player-0".into(),known_damage:Some(6),targets:vec![player_target("player-1")],..Default::default()});
        let mut bot=SimpleAi::default();bot.observe(view.clone());assert_eq!(bot.pending_damage("player-0","player-1"),6);
        view.stack[0].targets=vec![player_target("player-2")];bot.observe(view.clone());
        assert_eq!(bot.pending_damage("player-0","player-1"),0);assert_eq!(bot.pending_damage("player-0","player-2"),6);
        let threat:TargetRef=serde_json::from_value(json!({"id":"stack-1","kind":"spell","intent":"counter"})).unwrap();
        view.stack.push(StackObjectDto{id:"stack-2".into(),controller_id:"player-1".into(),targets:vec![threat],..Default::default()});bot.observe(view.clone());assert_eq!(bot.pending_damage("player-0","player-2"),0);
        view.stack.pop();view.stack[0].known_damage=None;bot.observe(view.clone());assert_eq!(bot.pending_damage("player-0","player-2"),0);
        view.stack.clear();bot.observe(view);assert_eq!(bot.pending_damage("player-0","player-2"),0);
    }
    #[test]
    fn selected_damage_target_is_always_in_the_provided_legal_set() {
        let mut view=GameViewDto::default();
        for i in 0..4 {let p:PlayerDto=serde_json::from_value(json!({"id":format!("player-{i}"),"name":"player","status":"playing","isHuman":false,"life":40,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap();view.players.push(p);}
        view.stack.push(StackObjectDto{id:"stack-1".into(),controller_id:"player-0".into(),known_damage:Some(40),targets:vec![player_target("player-2")],..Default::default()});
        let mut bot=SimpleAi::default();bot.observe(view);
        // The omitted opponent may be protected or otherwise ineligible. The
        // engine supplies the authoritative legal candidates, not the bot.
        let prompt:AgentPrompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","input":{"type":"chooseBoardTargets","presentation":{"title":"Damage","targets":[]},"candidates":[{"id":"player-2","kind":"player"},{"id":"player-3","kind":"player"}],"hostile":true,"intent":"damage","minTargets":1,"maxTargets":1,"chosenTargets":0,"cancellable":false}})).unwrap();
        let output=serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();assert_eq!(output["output"]["chosen"][0]["id"],"player-3","{output}");
    }
}

#[cfg(test)]
mod repair_damage_policy_tests {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{CardDto, CardView, GameViewDto, PlayerDto, StackObjectDto, ZoneDto, ZoneKind};
    use manabrew_protocol::prompts::common::TargetRef;
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;

    fn target(id:&str, kind:&str)->TargetRef {serde_json::from_value(json!({"id":id,"kind":kind,"intent":"damage"})).unwrap()}
    fn board()->GameViewDto {
        let mut view=GameViewDto::default();
        for i in 0..2 {let p:PlayerDto=serde_json::from_value(json!({"id":format!("player-{i}"),"name":"player","status":"playing","isHuman":false,"life":40,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap();view.players.push(p);}
        let card=CardDto{id:"card-1".into(),controller_id:"player-1".into(),types:vec!["Creature".into()],power:Some("8".into()),toughness:Some("6".into()),..Default::default()};
        view.zones.push(ZoneDto{zone:ZoneKind::Battlefield,owner_id:"player-1".into(),cards:vec![CardView::Visible(card)],count:1});view
    }
    fn choose(bot:&mut SimpleAi)->String {
        let p:AgentPrompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","knownDamage":6,"input":{"type":"chooseBoardTargets","presentation":{"title":"Damage","targets":[]},"candidates":[{"id":"player-0","kind":"player"},{"id":"player-1","kind":"player"},{"id":"card-1","kind":"card"}],"hostile":true,"intent":"damage","minTargets":1,"maxTargets":1,"chosenTargets":0,"cancellable":false}})).unwrap();
        serde_json::to_value(bot.decide(p).unwrap()).unwrap()["output"]["chosen"][0]["id"].as_str().unwrap().to_string()
    }
    #[test]
    fn useful_removal_is_available_but_covered_creature_is_not_repeated() {
        let mut view=board();let mut bot=SimpleAi::default();bot.observe(view.clone());assert_eq!(choose(&mut bot),"card-1");
        view.stack.push(StackObjectDto{id:"stack-1".into(),controller_id:"player-0".into(),known_damage:Some(6),targets:vec![target("card-1","card")],..Default::default()});
        bot.observe(view);assert_eq!(choose(&mut bot),"player-1");
    }
    #[test]
    fn redirect_makes_damage_unknown_and_covered_enemy_still_beats_self() {
        let mut view=board();view.stack.push(StackObjectDto{id:"stack-1".into(),controller_id:"player-0".into(),known_damage:Some(40),targets:vec![target("player-1","player")],..Default::default()});
        let mut bot=SimpleAi::default();bot.observe(view.clone());assert_ne!(choose(&mut bot),"player-0");
        let redirected:TargetRef=serde_json::from_value(json!({"id":"stack-1","kind":"spell","intent":"hostile"})).unwrap();
        view.stack.push(StackObjectDto{id:"stack-2".into(),controller_id:"player-1".into(),targets:vec![redirected],..Default::default()});bot.observe(view);assert_eq!(bot.pending_damage("player-0","player-1"),0);
    }
}

#[cfg(test)]
mod tribal_will_policy_regression {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{CardDto, CardView, GameViewDto, ZoneDto, ZoneKind};
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::json;

    fn card(id: &str, tribe: &str) -> CardDto {
        CardDto { id: id.into(), controller_id: "player-0".into(), owner_id: "player-0".into(),
            types: vec!["Creature".into()], subtypes: vec![tribe.into()], ..Default::default() }
    }
    fn choose(source: &str, title: &str, options: &[&str], max: usize, view: GameViewDto) -> Vec<usize> {
        let mut bot = SimpleAi::default(); bot.observe(view);
        let opts: Vec<_> = options.iter().map(|label| json!({"label":label,"weight":1,"canRepeat":false})).collect();
        let prompt: AgentPrompt = serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0",
            "sourceCard":{"id":"card-99","identity":{"name":source}},
            "input":{"type":"chooseFromSelection","presentation":{"title":title,"targets":[]},
                "options":opts,"minTotal":1,"maxTotal":max}})).unwrap();
        serde_json::from_value(serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"]["chosenIndices"].clone()).unwrap()
    }
    fn view(tribe: &str, attached: bool) -> GameViewDto {
        let mut v=GameViewDto::default(); let mut c=card("card-1",tribe);
        if attached { c.attachment_ids.push("card-3".into()); }
        v.zones.push(ZoneDto { zone: ZoneKind::Battlefield, owner_id:"player-0".into(), cards:vec![CardView::Visible(c)], count:1 }); v
    }
    #[test]
    fn tribe_choice_uses_relevant_creatures_and_does_not_force_dragons() {
        for (source, tribe, expected) in [("Roaming Throne","Dragon",1),("Herald's Horn","Angel",2),("Roaming Throne","Dinosaur",3)] {
            assert_eq!(choose(source,"Choose a Creature type", &["Advisor","Dragon","Angel","Dinosaur"],1,view(tribe,false)),vec![expected]);
        }
        let mut mixed=view("Dragon",false);
        if let CardView::Visible(ref mut c)=mixed.zones[0].cards[0] {c.subtypes.push("Spirit".into());c.text="Whenever another Dragon enters, copy it.".into();}
        assert_eq!(choose("Roaming Throne","Choose a Creature type", &["Advisor","Spirit","Dragon"],1,mixed),vec![2]);
        assert_eq!(choose("Other","Choose a Creature type", &["Advisor","Dragon"],1,view("Dragon",false)),vec![0]);
        assert_eq!(choose("Herald's Horn","Choose a Creature type", &["Advisor","Dragon"],1,GameViewDto::default()),vec![0]);
        let mut substring=view("Elf",false);
        if let CardView::Visible(ref mut c)=substring.zones[0].cards[0] {c.subtypes.push("Warrior".into());c.text="It protects itself and other Warriors.".into();}
        assert_eq!(choose("Herald's Horn","Choose a Creature type", &["Elf","Warrior"],1,substring),vec![1]);
    }
    #[test]
    fn will_uses_both_only_when_available_and_safe() {
        let modes=["Flying and double strike", "Lifelink and protection"];
        assert_eq!(choose("Akroma's Will","Choose Mode",&modes,2,view("Angel",false)),vec![0,1]);
        assert_eq!(choose("Akroma's Will","Choose Mode",&modes,1,view("Angel",false)),vec![0]);
        assert_eq!(choose("Akroma's Will","Choose Mode",&modes,2,view("Angel",true)),vec![0]);
        assert_eq!(choose("Other","Choose Mode",&modes,2,view("Angel",false)),vec![0]);
    }
}

/// Execution-audit policy regressions (Collection Plan DEV-060, DEV-062 and
/// DEV-063): each rule has positive and contrary controls.
#[cfg(test)]
mod execution_audit_policy {
    use super::SimpleAi;
    use crate::agent::BotAgent;
    use manabrew_protocol::game::{CardDto, CardView, GameViewDto, PlayerDto, StackObjectDto, StepKind, ZoneDto, ZoneKind};
    use manabrew_protocol::transport::AgentPrompt;
    use serde_json::{json, Value};

    fn player(id: usize, life: i32) -> PlayerDto {
        serde_json::from_value(json!({"id":format!("player-{id}"),"name":"p","status":"playing","isHuman":false,"life":life,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap()
    }
    fn creature(id: &str, controller: usize, pt: (i32, i32), cmc: i32, text: &str) -> CardDto {
        CardDto { id: id.into(), controller_id: format!("player-{controller}"), owner_id: format!("player-{controller}"), types: vec!["Creature".into()],
            power: Some(pt.0.to_string()), toughness: Some(pt.1.to_string()), cmc, text: text.into(), ..Default::default() }
    }
    fn land(id: &str, controller: usize, subtype: &str) -> CardDto {
        CardDto { id: id.into(), controller_id: format!("player-{controller}"), owner_id: format!("player-{controller}"), types: vec!["Land".into()], subtypes: vec![subtype.into()], ..Default::default() }
    }
    fn view(active: usize, step: StepKind, zones: Vec<(ZoneKind, usize, Vec<CardDto>)>) -> GameViewDto {
        let mut view = GameViewDto::default();
        view.active_player_id = format!("player-{active}");
        view.step = step;
        for i in 0..4 { view.players.push(player(i, 40)); }
        for (zone, owner, cards) in zones {
            view.zones.push(ZoneDto { zone, owner_id: format!("player-{owner}"), count: cards.len(), cards: cards.into_iter().map(CardView::Visible).collect() });
        }
        view
    }
    fn targets(bot: &mut SimpleAi, source: &str, text: &str, intent: &str, ids: &[&str], min: i32, cancellable: bool) -> Value {
        let candidates: Vec<_> = ids.iter().map(|id| json!({"id":id,"kind":"card"})).collect();
        let prompt: AgentPrompt = serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","sourceCard":{"id":source,"text":text},
            "sourceAbilityText":text,"input":{"type":"chooseBoardTargets","presentation":{"title":"Exile","targets":[]},"candidates":candidates,
            "hostile":true,"intent":intent,"minTargets":min,"maxTargets":1,"chosenTargets":0,"cancellable":cancellable}})).unwrap();
        serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone()
    }
    fn confirm(bot: &mut SimpleAi, source: &str, title: &str) -> bool {
        let prompt: AgentPrompt = serde_json::from_value(json!({"promptId":2,"decidingPlayerId":"player-0","sourceCard":{"id":source},
            "input":{"type":"chooseBoolean","confirmLabel":"Accept","denyLabel":"Decline","presentation":{"title":title,"targets":[]}}})).unwrap();
        serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"]["value"].as_bool().unwrap()
    }
    const BLINK: &str = "Exile target creature you control, then return that card to the battlefield under its owner's control.";
    const ANGEL: &str = "When CARDNAME enters, you may exile target non-Angel creature you control, then return that creature to the battlefield under your control.";
    const AANG: &str = "Flying\nWhen Aang enters, look at the top five cards of your library.";

    fn board() -> Vec<CardDto> {
        let mut token = creature("card-3", 0, (1, 1), 0, "When this creature enters, draw a card.");
        token.identity.is_token = true;
        vec![creature("card-1", 0, (3, 3), 5, AANG), creature("card-2", 0, (0, 1), 1, "{T}: Add {W}."), token]
    }

    fn number(bot: &mut SimpleAi, text: &str, max: i32) -> i64 {
        let prompt: AgentPrompt = serde_json::from_value(json!({"promptId":3,"decidingPlayerId":"player-0","sourceCard":{"id":"card-9","text":text},
            "input":{"type":"chooseNumber","presentation":{"title":"Choose a number","targets":[]},"min":0,"max":max}})).unwrap();
        serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"]["chosenNumber"].as_i64().unwrap()
    }

    /// Plague of Vermin repeats until no one pays, so paying 1 every round loops.
    #[test]
    fn repeated_life_payment_pays_once_with_a_cushion() {
        const PLAGUE: &str = "Starting with you, each player may pay any amount of life. Repeat this process until no one pays life.";
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![]));
        assert_eq!(number(&mut bot, PLAGUE, 40), 10, "first round keeps 20 life plus half the excess");
        assert_eq!(number(&mut bot, PLAGUE, 30), 0, "later rounds of the same resolution pay nothing");
        let mut other = SimpleAi::default();
        other.observe(view(0, StepKind::Main1, vec![]));
        assert_eq!(number(&mut other, "Choose a number.", 5), 1, "other number prompts are unchanged");
    }

    #[test]
    fn reuse_targets_the_best_entry_creature_and_skips_tokens() {
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, board())]));
        assert_eq!(targets(&mut bot, "card-9", BLINK, "exile", &["card-2", "card-3", "card-1"], 1, false)["chosen"][0]["id"], "card-1");
        // No useful target: a voluntary blink spell is not wasted; a mandatory target stays harmless.
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, board()[1..].to_vec())]));
        bot.pending_voluntary_cast = Some("card-9".into());
        assert_eq!(targets(&mut bot, "card-9", BLINK, "exile", &["card-2", "card-3"], 1, true)["type"], "cancel");
        bot.pending_voluntary_cast = None;
        assert_eq!(targets(&mut bot, "card-9", BLINK, "exile", &["card-2", "card-3"], 1, false)["chosen"][0]["id"], "card-2");
        // "Up to one": no useful target means no target.
        assert_eq!(targets(&mut bot, "card-9", BLINK, "exile", &["card-2", "card-3"], 0, false)["chosen"], json!([]));
    }

    #[test]
    fn optional_reuse_is_accepted_only_for_a_useful_target() {
        for (target, attacking, threat, expected) in [("card-1", false, false, true), ("card-3", false, false, false), ("card-2", false, false, false),
            ("card-1", true, false, false), ("card-2", false, true, true)] {
            let mut cards = board();
            cards[0].is_attacking = attacking;
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, cards)]);
            view.stack.push(StackObjectDto { id: "stack-1".into(), source_id: "card-8".into(), controller_id: "player-0".into(),
                targets: serde_json::from_value(json!([{"id":target,"kind":"card"}])).unwrap(), ..Default::default() });
            if threat {
                view.stack.push(StackObjectDto { id: "stack-2".into(), source_id: "card-20".into(), controller_id: "player-1".into(),
                    text: "Destroy target creature.".into(), targets: serde_json::from_value(json!([{"id":target,"kind":"card"}])).unwrap(), ..Default::default() });
            }
            let mut bot = SimpleAi::default();
            bot.observe(view);
            assert_eq!(confirm(&mut bot, "card-8", ANGEL), expected, "target {target} attacking {attacking} threat {threat}");
        }
        // Without a visible stack target, the remembered chosen target decides.
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::EndOfTurn, vec![(ZoneKind::Battlefield, 0, board())]));
        targets(&mut bot, "card-8", BLINK, "exile", &["card-1", "card-2"], 1, false);
        assert!(confirm(&mut bot, "card-8", "At the beginning of your end step, you may exile another target creature you control, then return it to the battlefield under its owner's control."));
    }

    #[test]
    fn transformed_front_face_entry_is_remembered() {
        let mut bot = SimpleAi::default();
        let mut aang = board()[0].clone();
        aang.is_double_faced = true;
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![aang.clone()])]));
        aang.is_transformed = true;
        aang.text = "Flying\nLand creatures you control have vigilance.".into();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![aang, board()[1].clone()])]));
        assert_eq!(targets(&mut bot, "card-9", BLINK, "exile", &["card-2", "card-1"], 1, false)["chosen"][0]["id"], "card-1");
    }

    #[test]
    fn untap_payment_requires_own_turn_tapped_creature_and_mana() {
        const DANCE: &str = "At the beginning of the upkeep of enchanted creature's controller, that player may pay {1}{B}. If the player does, untap that creature.";
        for (active, tapped, lands, expected) in [(0, true, vec!["Swamp", "Swamp"], true), (1, true, vec!["Swamp", "Swamp"], false),
            (0, false, vec!["Swamp", "Swamp"], false), (0, true, vec!["Swamp"], false), (0, true, vec!["Plains", "Plains"], false)] {
            let mut dance = CardDto { id: "card-10".into(), controller_id: "player-0".into(), types: vec!["Enchantment".into()], attached_to: Some("card-11".into()), ..Default::default() };
            dance.subtypes = vec!["Aura".into()];
            let mut demon = creature("card-11", 0, (6, 6), 6, "Flying");
            demon.tapped = tapped;
            let mut cards = vec![dance, demon];
            cards.extend(lands.iter().enumerate().map(|(i, subtype)| land(&format!("card-{}", 20 + i), 0, subtype)));
            let mut bot = SimpleAi::default();
            bot.observe(view(active, StepKind::Upkeep, vec![(ZoneKind::Battlefield, 0, cards)]));
            assert_eq!(confirm(&mut bot, "card-10", DANCE), expected, "active {active} tapped {tapped} lands {lands:?}");
        }
    }

    #[test]
    fn own_graveyard_return_is_accepted_without_an_extra_cost() {
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![]));
        assert!(confirm(&mut bot, "card-5", "Landfall — Whenever a land you control enters, you may return target nonland permanent card from your graveyard to your hand."));
        assert!(!confirm(&mut bot, "card-6", "At the beginning of your upkeep, you may pay 2 life. If you do, you may return target creature card from your graveyard to your hand."));
    }

    #[test]
    fn reanimation_auras_value_the_creature_for_its_new_controller() {
        let aura = "Enchant creature card in a graveyard\nWhen Animate Dead enters, return enchanted creature card to the battlefield under your control.";
        let mut razaketh = creature("card-30", 0, (8, 8), 7, "Flying, trample");
        razaketh.keywords = vec!["Flying".into(), "Trample".into()];
        let mut ruins = creature("card-31", 1, (5, 7), 7, "Flying");
        ruins.keywords = vec!["Flying".into()];
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Graveyard, 0, vec![razaketh]), (ZoneKind::Graveyard, 1, vec![ruins.clone()])]));
        assert_eq!(targets(&mut bot, "card-40", aura, "hostile", &["card-31", "card-30"], 1, true)["chosen"][0]["id"], "card-30");
        // A stronger opponent creature is still worth stealing.
        let mut lyra = creature("card-32", 1, (5, 5), 5, "Flying, first strike, lifelink");
        lyra.keywords = vec!["Flying".into(), "First strike".into(), "Lifelink".into()];
        let mut shepherd = creature("card-33", 0, (4, 4), 4, "Flying");
        shepherd.keywords = vec!["Flying".into()];
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Graveyard, 0, vec![shepherd.clone()]), (ZoneKind::Graveyard, 1, vec![lyra])]));
        assert_eq!(targets(&mut bot, "card-40", aura, "hostile", &["card-33", "card-32"], 1, true)["chosen"][0]["id"], "card-32");
        // A tribal entry payoff tips a close choice toward the matching creature.
        let mut demon = shepherd.clone();
        demon.subtypes = vec!["Demon".into()];
        let mut flier = creature("card-34", 1, (4, 5), 4, "Flying");
        flier.keywords = vec!["Flying".into()];
        let payoff = creature("card-35", 0, (6, 5), 6, "Whenever another Demon you control enters, it deals damage equal to its power to any target.");
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![payoff]), (ZoneKind::Graveyard, 0, vec![demon]), (ZoneKind::Graveyard, 1, vec![flier])]));
        assert_eq!(targets(&mut bot, "card-40", aura, "hostile", &["card-34", "card-33"], 1, true)["chosen"][0]["id"], "card-33");
    }

    #[test]
    fn self_copy_destroy_targets_own_entry_creature_or_is_not_cast() {
        let saw = "Destroy target creature. If that creature dies this way, its controller creates two tokens that are copies of that creature, except their base power is half that creature's power.";
        let own = creature("card-50", 0, (6, 5), 6, "Flying\nWhen Be'lakor enters, you draw X cards and you lose X life.");
        let enemy = creature("card-51", 1, (4, 4), 4, "When Fire Lord Azula enters, deal damage.");
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![own]), (ZoneKind::Battlefield, 1, vec![enemy.clone()])]));
        bot.pending_voluntary_cast = Some("card-52".into());
        assert_eq!(targets(&mut bot, "card-52", saw, "destroy", &["card-51", "card-50"], 1, true)["chosen"][0]["id"], "card-50");
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 1, vec![enemy])]));
        bot.pending_voluntary_cast = Some("card-52".into());
        assert_eq!(targets(&mut bot, "card-52", saw, "destroy", &["card-51"], 1, true)["type"], "cancel");
    }

    fn act(bot: &mut SimpleAi, actions: Value) -> Value {
        let prompt: AgentPrompt = serde_json::from_value(json!({"promptId":3,"decidingPlayerId":"player-0","input":{"type":"chooseAction","actions":actions}})).unwrap();
        serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone()
    }

    fn spell_card(id: &str, mana_cost: &str, cmc: i32, text: &str) -> CardDto {
        CardDto { id: id.into(), controller_id: "player-0".into(), owner_id: "player-0".into(), types: vec!["Instant".into()],
            mana_cost: mana_cost.into(), cmc, text: text.into(), ..Default::default() }
    }
    fn cast_choice(bot: &mut SimpleAi, card: &str) -> Value {
        act(bot, json!([{"type":"cast","id":format!("cast:{card}:normal"),"cardId":card,"mode":{"type":"normal"},"label":"Cast normally"}]))["type"].clone()
    }

    /// DEV-046: a blink spell is started for a permanent that gains from re-entering, and not
    /// started (then cancelled at its target prompt) when nothing does.
    #[test]
    fn a_blink_spell_is_cast_for_an_entry_creature_and_not_for_a_vanilla_one() {
        const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
        for (battlefield, threatened, expected) in [
            (creature("card-1", 0, (3, 3), 5, "Flying\nWhen Aang enters, look at the top five cards of your library."), false, "act"),
            (creature("card-1", 0, (2, 2), 3, "Flying"), false, "pass"),
            (creature("card-1", 0, (2, 2), 3, "Flying"), true, "act"),
        ] {
            let mut lands: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 20 + i), 0, "Plains")).collect();
            lands.push(battlefield);
            let hand = vec![spell_card("card-9", "{W}", 1, CLOUDSHIFT)];
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, lands), (ZoneKind::Hand, 0, hand)]);
            if threatened {
                view.stack.push(StackObjectDto { id: "stack-1".into(), controller_id: "player-1".into(), is_spell: true,
                    text: "Destroy target creature.".into(),
                    targets: serde_json::from_value(json!([{"id":"card-1","kind":"card"}])).unwrap(), ..Default::default() });
            }
            let mut bot = SimpleAi::default();
            bot.observe(view);
            assert_eq!(cast_choice(&mut bot, "card-9"), expected, "{threatened}");
        }
    }

    /// The blink target prompt still picks the entry creature, including a commander.
    #[test]
    fn a_blink_spell_targets_the_commander_with_an_entry_ability() {
        const EPHEMERATE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.";
        let aang = creature("card-1", 0, (3, 3), 5, "When Aang enters, look at the top five cards of your library.");
        let vanilla = creature("card-2", 0, (1, 1), 1, "");
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![vanilla, aang])]));
        bot.pending_voluntary_cast = Some("card-9".into());
        let out = targets(&mut bot, "card-9", EPHEMERATE, "exile", &["card-2", "card-1"], 1, true);
        assert_eq!(out["chosen"][0]["id"], "card-1");
    }

    /// DEV-046: a counterspell needs an opposing spell; an ability on the stack, or only our own
    /// spell there, is not a target worth starting a cast for.
    #[test]
    fn a_counterspell_is_cast_for_an_opposing_spell_only() {
        for (controller, is_spell, expected) in [("player-1", true, "act"), ("player-1", false, "pass"), ("player-0", true, "pass")] {
            let islands: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 20 + i), 0, "Island")).collect();
            let hand = vec![spell_card("card-9", "{U}{U}", 2, "Counter target spell.")];
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, islands), (ZoneKind::Hand, 0, hand)]);
            view.stack.push(StackObjectDto { id: "stack-1".into(), controller_id: controller.into(), is_spell,
                text: "Draw a card.".into(), source_card: Some(spell_card("card-30", "{1}{U}", 2, "Draw a card.")), ..Default::default() });
            let mut bot = SimpleAi::default();
            bot.observe(view);
            assert_eq!(cast_choice(&mut bot, "card-9"), expected, "{controller} spell={is_spell}");
        }
    }

    /// The counter's target prompt takes the opposing spell rather than cancelling.
    #[test]
    fn a_counterspell_targets_the_opposing_spell() {
        let mut view = view(0, StepKind::Main1, vec![]);
        view.stack.push(StackObjectDto { id: "stack-1".into(), controller_id: "player-1".into(), is_spell: true,
            text: "Draw a card.".into(), source_card: Some(spell_card("card-30", "{1}{U}", 2, "Draw a card.")), ..Default::default() });
        let mut bot = SimpleAi::default();
        bot.observe(view);
        bot.pending_voluntary_cast = Some("card-9".into());
        let prompt: AgentPrompt = serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","sourceCard":{"id":"card-9","text":"Counter target spell."},
            "sourceAbilityText":"Counter target spell.","input":{"type":"chooseBoardTargets","presentation":{"title":"Counter","targets":[]},
            "candidates":[{"id":"stack-1","kind":"spell"}],"hostile":true,"intent":"counter","minTargets":1,"maxTargets":1,"chosenTargets":0,"cancellable":true}})).unwrap();
        let out = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone();
        assert_eq!(out["chosen"][0]["id"], "stack-1");
    }

    /// A removal spell with exactly one legal target (an opposing creature) completes.
    #[test]
    fn removal_with_one_legal_target_takes_it() {
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 1, vec![creature("card-7", 1, (4, 4), 5, "Flying")])]));
        bot.pending_voluntary_cast = Some("card-9".into());
        let out = targets(&mut bot, "card-9", "Destroy target creature.", "destroy", &["card-7"], 1, true);
        assert_eq!(out["chosen"][0]["id"], "card-7");
    }

    /// DEV-072: a source that costs life is not counted when paying would end the game, so a
    /// cast that needs it is not started and then refused at the payment.
    #[test]
    fn a_life_costing_source_is_not_counted_at_low_life() {
        let confluence = |id: &str| { let mut c = land(id, 0, ""); c.subtypes.clear();
            c.text = "{T}, Pay 1 life: Add one mana of any color.".into(); c };
        for (life, expected) in [(1, 0), (5, 1)] {
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![confluence("card-1")])]);
            view.players[0].life = life;
            let mut bot = SimpleAi::default();
            bot.observe(view);
            assert_eq!(bot.mana_sources("player-0").2, expected, "life {life}");
        }
    }

    /// DEV-071: a stack item of ours that already aims at a target in a given way.
    fn own_item(id: &str, text: &str, target_id: &str, kind: &str, intent: &str) -> StackObjectDto {
        let target: manabrew_protocol::prompts::common::TargetRef = serde_json::from_value(json!({"id":target_id,"kind":kind,"intent":intent})).unwrap();
        StackObjectDto { id: id.into(), controller_id: "player-0".into(), text: text.into(), source_ability_text: Some(text.into()),
            targets: vec![target], ..Default::default() }
    }

    /// DEV-071: a second damage trigger skips the creature our first one already kills.
    #[test]
    fn a_second_damage_trigger_picks_a_different_target() {
        let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 1, vec![
            creature("card-7", 1, (1, 1), 1, ""), creature("card-8", 1, (1, 1), 1, "")])]);
        view.stack.push(own_item("stack-1", "CARDNAME deals 1 damage to target creature.", "card-7", "card", "damage"));
        let mut bot = SimpleAi::default();
        bot.observe(view);
        let out = targets(&mut bot, "card-9", "CARDNAME deals 1 damage to target creature.", "damage", &["card-7", "card-8"], 1, false);
        assert_eq!(out["chosen"][0]["id"], "card-8");
    }

    /// DEV-071: removal skips a permanent our stack already exiles, and with nothing else left
    /// still takes it.
    #[test]
    fn removal_skips_a_permanent_already_being_removed() {
        let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 1, vec![
            creature("card-7", 1, (6, 6), 6, ""), creature("card-8", 1, (1, 1), 1, "")])]);
        view.stack.push(own_item("stack-1", "Destroy target creature.", "card-7", "card", "destroy"));
        let mut bot = SimpleAi::default();
        bot.observe(view);
        let out = targets(&mut bot, "card-9", "Destroy target creature.", "destroy", &["card-7", "card-8"], 1, false);
        assert_eq!(out["chosen"][0]["id"], "card-8");
        let out = targets(&mut bot, "card-9", "Destroy target creature.", "destroy", &["card-7"], 1, false);
        assert_eq!(out["chosen"][0]["id"], "card-7");
    }

    /// DEV-071: a doubled blink takes a different creature than the blink already on the stack.
    #[test]
    fn a_second_blink_takes_a_different_creature() {
        const EPHEMERATE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.";
        let first = creature("card-1", 0, (3, 3), 5, "When Aang enters, look at the top five cards of your library.");
        let second = creature("card-2", 0, (2, 2), 3, "When this creature enters, draw a card.");
        let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![first, second])]);
        view.stack.push(own_item("stack-1", EPHEMERATE, "card-1", "card", "exile"));
        let mut bot = SimpleAi::default();
        bot.observe(view);
        bot.pending_voluntary_cast = Some("card-9".into());
        let out = targets(&mut bot, "card-9", EPHEMERATE, "exile", &["card-1", "card-2"], 1, true);
        assert_eq!(out["chosen"][0]["id"], "card-2");
    }

    /// DEV-071: a second counterspell is not cast at a spell two of ours already counter, and
    /// one is still cast when only one does.
    #[test]
    fn a_counter_is_not_cast_at_a_spell_already_countered() {
        for (own_counters, expected) in [(2, "pass"), (1, "act")] {
            let islands: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 20 + i), 0, "Island")).collect();
            let hand = vec![spell_card("card-9", "{U}{U}", 2, "Counter target spell.")];
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, islands), (ZoneKind::Hand, 0, hand)]);
            view.stack.push(StackObjectDto { id: "stack-1".into(), controller_id: "player-1".into(), is_spell: true,
                text: "Draw a card.".into(), source_card: Some(spell_card("card-30", "{1}{U}", 2, "Draw a card.")), ..Default::default() });
            for i in 0..own_counters {
                let mut item = own_item(&format!("stack-{}", i + 2), "Counter target spell unless its controller pays {1}.", "stack-1", "spell", "counter");
                item.is_spell = true;
                view.stack.push(item);
            }
            let mut bot = SimpleAi::default();
            bot.observe(view);
            assert_eq!(cast_choice(&mut bot, "card-9"), expected, "{own_counters} counters already aimed");
        }
    }

    /// DEV-087: a stack item of another seat, aimed at an object.
    fn seat_item(id: &str, controller: usize, text: &str, target_id: &str, kind: &str, intent: &str) -> StackObjectDto {
        let mut item = own_item(id, text, target_id, kind, intent);
        item.controller_id = format!("player-{controller}");
        item.is_spell = true;
        item
    }

    /// DEV-087: a counterspell is not cast at a spell that a second opponent's counter already
    /// takes, unless that counter cannot take it or is itself about to be countered.
    #[test]
    fn a_counter_is_not_cast_at_a_spell_another_seat_already_counters() {
        const SWAN_SONG: &str = "Counter target enchantment, instant, or sorcery spell. Its controller creates a 2/2 blue Bird creature token with flying.";
        for (other, countered_in_turn, expected) in [
            (None, false, "act"),
            (Some("Counter target spell."), false, "pass"),
            (Some(SWAN_SONG), false, "pass"),
            (Some("Counter target creature spell."), false, "act"),
            (Some("Counter target spell."), true, "act"),
        ] {
            let islands: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 20 + i), 0, "Island")).collect();
            let hand = vec![spell_card("card-9", "{U}{U}", 2, "Counter target spell.")];
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, islands), (ZoneKind::Hand, 0, hand)]);
            view.stack.push(StackObjectDto { id: "stack-1".into(), controller_id: "player-1".into(), is_spell: true,
                text: "Draw a card.".into(), source_card: Some(spell_card("card-30", "{1}{U}", 2, "Draw a card.")), ..Default::default() });
            if let Some(text) = other {
                view.stack.push(seat_item("stack-2", 2, text, "stack-1", "spell", "counter"));
                if countered_in_turn {
                    // The spell's own controller answers the answer.
                    view.stack.push(seat_item("stack-3", 1, "Counter target spell.", "stack-2", "spell", "counter"));
                }
            }
            let mut bot = SimpleAi::default();
            bot.observe(view);
            assert_eq!(cast_choice(&mut bot, "card-9"), expected, "{other:?}, answered: {countered_in_turn}");
        }
    }

    /// DEV-087: a removal spell is not aimed at a permanent another opponent's removal already
    /// takes; it still is when nothing else is left to choose from, and with no removal pending.
    #[test]
    fn a_removal_spell_is_not_cast_at_a_permanent_another_seat_already_removes() {
        const PATH: &str = "Exile target creature. Its controller may search their library for a basic land card.";
        for (pending, expected) in [(false, "boardTargets"), (true, "cancel")] {
            let mut enemy = creature("card-7", 1, (3, 3), 4, "");
            enemy.zone = Some(ZoneKind::Battlefield);
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 1, vec![enemy])]);
            if pending {
                view.stack.push(seat_item("stack-1", 2, PATH, "card-7", "card", "exile"));
            }
            let mut bot = SimpleAi::default();
            bot.observe(view);
            bot.pending_voluntary_cast = Some("card-9".into());
            assert_eq!(targets(&mut bot, "card-9", PATH, "exile", &["card-7"], 1, true)["type"], expected, "pending: {pending}");
        }
    }

    /// DEV-086: the cast check and the target prompt use one rule for a blink spell. Eerie
    /// Interlude is not cast when the only creature worth blinking is already being exiled by
    /// our Ephemerate.
    #[test]
    fn a_blink_spell_is_not_cast_for_a_creature_our_stack_already_blinks() {
        const EERIE: &str = "Exile any number of target creatures you control. Return those cards to the battlefield under their owner's control at the beginning of the next end step.";
        const EPHEMERATE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.";
        for (pending, expected) in [(false, "act"), (true, "pass")] {
            let mut lands: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 20 + i), 0, "Plains")).collect();
            lands.push(creature("card-66", 0, (3, 3), 5, "When this creature enters, draw a card."));
            let hand = vec![spell_card("card-9", "{W}", 1, EERIE)];
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, lands), (ZoneKind::Hand, 0, hand)]);
            if pending {
                view.stack.push(own_item("stack-1", EPHEMERATE, "card-66", "card", "exile"));
            }
            let mut bot = SimpleAi::default();
            bot.observe(view);
            assert_eq!(cast_choice(&mut bot, "card-9"), expected, "pending: {pending}");
        }
    }

    /// DEV-071: the copy of a spell that takes the same target as the original (Azula's copy of
    /// Chaos Warp, which has no damage or destroy words) chooses a new one.
    #[test]
    fn a_copy_aimed_where_the_original_aims_gets_new_targets() {
        let mut view = view(0, StepKind::Main1, vec![]);
        let mut original = own_item("stack-1", "The owner of target permanent shuffles it into their library.", "card-7", "card", "bounce");
        original.is_spell = true;
        let mut copy = own_item("stack-2", "The owner of target permanent shuffles it into their library.", "card-7", "card", "bounce");
        copy.is_spell = true;
        view.stack.push(original);
        view.stack.push(copy);
        let mut bot = SimpleAi::default();
        bot.observe(view);
        assert!(confirm(&mut bot, "card-1", "Choose new targets for the copy?"));
    }

    /// A cast cancelled at target selection is not started again against the same board.
    #[test]
    fn a_copied_spell_gets_new_targets_instead_of_fizzling_the_original() {
        let stack_with = |text: &str| {
            let mut view = view(0, StepKind::Main1, vec![]);
            view.stack.push(StackObjectDto { id: "stack-1".into(), controller_id: "player-0".into(), is_spell: true,
                text: text.into(), ..Default::default() });
            let mut bot = SimpleAi::default();
            bot.observe(view);
            bot
        };
        let mut damage = stack_with("Firebending Lesson deals 2 damage to target creature.");
        assert!(confirm(&mut damage, "card-1", "Choose new targets for the copy?"));
        // Retargeting a copy of Expansion would only copy the copy again, without end.
        let mut copier = stack_with("Copy target instant or sorcery spell with mana value 4 or less. You may choose new targets for the copy.");
        copier.view.as_mut().unwrap().stack.push(StackObjectDto { id: "stack-2".into(), controller_id: "player-0".into(), is_spell: true,
            text: "Lightning Bolt deals 3 damage to any target.".into(), ..Default::default() });
        assert!(!confirm(&mut copier, "card-1", "Choose new targets for the copy?"));
    }

    #[test]
    fn a_counter_clause_names_the_spells_it_can_take() {
        let types = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let (creature, instant, enchant) = (types(&["Creature"]), types(&["Instant"]), types(&["Enchantment"]));
        let allows = |text: &str, types: &[String]| SimpleAi::counter_clause_allows(text, types, true);
        assert!(allows("you may exile a blue card rather than pay. counter target spell.", &creature));
        assert!(!allows("counter target noncreature spell.", &creature));
        assert!(allows("counter target noncreature spell.", &instant));
        assert!(allows("counter target creature spell.", &creature) && !allows("counter target creature spell.", &instant));
        assert!(allows("counter target enchantment, instant, or sorcery spell.", &enchant));
        assert!(!allows("counter target enchantment, instant, or sorcery spell.", &creature));
        assert!(allows("counter target multicolored spell.", &creature), "an unknown qualifier is left to the target prompt");
    }

    /// A cast the engine could not start (the card is still in hand at the next action prompt)
    /// is not chosen again against the same board.
    #[test]
    fn a_cast_that_never_reached_the_stack_is_not_chosen_again() {
        let lands: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 20 + i), 0, "Island")).collect();
        let mut hand_card = spell_card("card-9", "{U}", 1, "Draw a card.");
        hand_card.zone = Some(ZoneKind::Hand);
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, lands.clone()), (ZoneKind::Hand, 0, vec![hand_card.clone()])]));
        assert_eq!(cast_choice(&mut bot, "card-9"), "act");
        // The next action prompt: the card is still in hand and the stack is unchanged.
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, lands), (ZoneKind::Hand, 0, vec![hand_card])]));
        bot.attempted_actions.clear();
        assert_eq!(cast_choice(&mut bot, "card-9"), "pass");
    }

    #[test]
    fn a_cancelled_play_is_not_restarted_until_the_board_changes() {
        const BLINK: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![creature("card-2", 0, (0, 1), 1, "")])]));
        bot.pending_voluntary_cast = Some("card-9".into());
        assert_eq!(targets(&mut bot, "card-9", BLINK, "exile", &["card-2"], 1, true)["type"], "cancel");
        assert_eq!(bot.cancelled_plays.get("card-9"), Some(&bot.board_signature()));
        let before = bot.board_signature();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, vec![creature("card-2", 0, (0, 1), 1, ""), creature("card-3", 0, (1, 1), 1, "")])]));
        assert_ne!(bot.board_signature(), before, "a new permanent is a new board");
    }

    #[test]
    fn instants_are_not_cast_in_response_to_the_players_own_upkeep_trigger() {
        for (foreign, expected) in [(false, "pass"), (true, "act")] {
            let will = CardDto { id: "card-60".into(), controller_id: "player-0".into(), types: vec!["Instant".into()], mana_cost: "{2}{W}".into(), cmc: 3,
                text: "Choose one. Creatures you control gain flying, vigilance and double strike until end of turn.".into(), ..Default::default() };
            let lands: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 61 + i), 0, "Plains")).collect();
            let mut view = view(0, StepKind::Upkeep, vec![(ZoneKind::Battlefield, 0, lands), (ZoneKind::Hand, 0, vec![will])]);
            view.stack.push(StackObjectDto { id: "stack-1".into(), controller_id: if foreign { "player-1" } else { "player-0" }.into(),
                text: "At the beginning of your upkeep, lose 1 life.".into(), ..Default::default() });
            let mut bot = SimpleAi::default();
            bot.observe(view);
            assert_eq!(act(&mut bot, json!([{"type":"cast","id":"cast:card-60:normal","cardId":"card-60","mode":{"type":"normal"},"label":"Cast normally"}]))["type"], expected);
        }
    }

    #[test]
    fn a_lethal_mass_life_loss_spell_outranks_development() {
        let fraying = CardDto { id: "card-70".into(), controller_id: "player-0".into(), types: vec!["Sorcery".into()], mana_cost: "{4}{B}{B}".into(), cmc: 6,
            text: "Each player loses half their life, rounded up. Then each player sacrifices half the creatures they control, rounded up.".into(), ..Default::default() };
        let bowmasters = CardDto { id: "card-71".into(), controller_id: "player-0".into(), types: vec!["Creature".into()], mana_cost: "{1}{B}".into(), cmc: 2,
            power: Some("1".into()), toughness: Some("1".into()), text: "Flash".into(), ..Default::default() };
        let bloodletter = creature("card-72", 0, (2, 4), 4, "Flying\nIf an opponent would lose life during your turn, they lose twice that much life instead.");
        for (doubler, lives, expected) in [(true, [33, 40, 25, 40], "cast:card-70:normal"), (false, [33, 40, 25, 40], "cast:card-71:normal"),
            (false, [33, 1, 1, 1], "cast:card-70:normal"), (false, [33, 2, 1, 1], "cast:card-71:normal"), (true, [1, 40, 25, 40], "cast:card-71:normal")] {
            let mut battlefield: Vec<_> = (0..6).map(|i| land(&format!("card-{}", 80 + i), 0, "Swamp")).collect();
            if doubler { battlefield.push(bloodletter.clone()); }
            let mut view = view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, battlefield), (ZoneKind::Hand, 0, vec![fraying.clone(), bowmasters.clone()])]);
            for (player, life) in view.players.iter_mut().zip(lives) { player.life = life; }
            let mut bot = SimpleAi::default();
            bot.observe(view);
            let actions = json!([{"type":"cast","id":"cast:card-70:normal","cardId":"card-70","mode":{"type":"normal"},"label":"Cast normally"},
                {"type":"cast","id":"cast:card-71:normal","cardId":"card-71","mode":{"type":"normal"},"label":"Cast normally"}]);
            assert_eq!(act(&mut bot, actions)["actionId"], expected, "doubler {doubler} lives {lives:?}");
        }
    }

    // ---- Bot fidelity (DEV-075, DEV-076, DEV-077) ----

    fn with_library(mut view: GameViewDto, count: usize) -> GameViewDto {
        view.zones.push(ZoneDto { zone: ZoneKind::Library, owner_id: "player-0".into(), cards: vec![], count });
        view
    }
    fn ability(card: &str, cost: &str, description: &str) -> Value {
        json!([{"type":"activateAbility","id":format!("ability:{card}:0"),"cardId":card,"abilityIndex":0,
            "description":description,"isManaAbility":false,"cost":cost}])
    }
    const SAC_SELF: &str = "Sacrifice { amount: Literal(1), type_filter: \"CARDNAME\" }";

    #[test]
    fn a_fetch_land_is_cracked_on_its_own_turn_and_not_at_low_life() {
        let fetch = CardDto { id: "card-5".into(), controller_id: "player-0".into(), owner_id: "player-0".into(), types: vec!["Land".into()],
            text: "{T}, Pay 1 life, Sacrifice this land: Search your library for an Island or Mountain card.".into(), ..Default::default() };
        let cost = format!("{{T}}, Pay 1 life, {SAC_SELF}");
        let search = "Search your library for a Island or Mountain card, put it onto the battlefield, then shuffle.";
        for (step, life, expected) in [(StepKind::Main1, 40, "act"), (StepKind::Main1, 6, "pass"), (StepKind::Upkeep, 40, "pass")] {
            let mut v = with_library(view(0, step, vec![(ZoneKind::Battlefield, 0, vec![fetch.clone()])]), 60);
            v.players[0].life = life;
            let mut bot = SimpleAi::default();
            bot.observe(v);
            assert_eq!(act(&mut bot, ability("card-5", &cost, search))["type"], expected, "{step:?} life {life}");
        }
    }

    #[test]
    fn a_clue_is_cracked_with_spare_mana_in_the_second_main_phase_only() {
        let clue = CardDto { id: "card-6".into(), controller_id: "player-0".into(), owner_id: "player-0".into(), types: vec!["Artifact".into()], ..Default::default() };
        let cost = format!("{{2}}, {SAC_SELF}");
        for (step, expected) in [(StepKind::Main2, "act"), (StepKind::Main1, "pass")] {
            let mut bot = SimpleAi::default();
            bot.observe(with_library(view(0, step, vec![(ZoneKind::Battlefield, 0, vec![clue.clone()])]), 60));
            assert_eq!(act(&mut bot, ability("card-6", &cost, "Draw a card."))["type"], expected, "{step:?}");
        }
    }

    #[test]
    fn a_tutor_sacrifices_fodder_and_never_the_commander() {
        let razaketh = creature("card-7", 0, (5, 5), 8, "Pay 2 life, Sacrifice another creature: Search your library for a card.");
        let mut commander = creature("card-8", 0, (2, 2), 3, "");
        commander.supertypes = vec!["Legendary".into()];
        commander.commander_tax = Some(0);
        let mut token = creature("card-9", 0, (1, 1), 0, "");
        token.identity.is_token = true;
        let cost = "Pay 2 life, Sacrifice { amount: Literal(1), type_filter: \"Creature.Other\" }";
        let search = "Search your library for a card, put that card into your hand, then shuffle.";
        for (others, expected) in [(vec![commander.clone()], "pass"), (vec![commander.clone(), token], "act")] {
            let mut battlefield = vec![razaketh.clone()];
            battlefield.extend(others);
            let mut bot = SimpleAi::default();
            bot.observe(with_library(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, battlefield)]), 60));
            assert_eq!(act(&mut bot, ability("card-7", cost, search))["type"], expected);
        }
    }

    #[test]
    fn optional_gains_are_taken_and_costly_ones_weigh_their_cost() {
        const SEARCH: &str = "When CARDNAME enters, you may search your library for a creature card with mana value 1 or less, reveal it, put it into your hand.";
        const PAY_DRAW: &str = "Whenever the Ring tempts you, you may pay 2 life. If you do, draw a card.";
        const PAY_TEN: &str = "At the beginning of your upkeep, you may pay 10 life. If you do, draw a card.";
        const TOKEN: &str = "Whenever another nontoken creature you control dies, you may exile it. If you do, create a token that's a copy of it.";
        for (title, library, life, expected) in [(SEARCH, 60, 40, true), (SEARCH, 0, 40, false), (PAY_DRAW, 60, 40, true), (PAY_DRAW, 60, 10, false),
            (PAY_TEN, 60, 25, false), (TOKEN, 60, 40, true), ("Pay {4}?", 60, 40, false), ("Do you want to draw 2 card(s)?", 60, 40, true),
            ("Do you want to draw 2 card(s)?", 3, 40, false), ("Search kaalia's library?", 60, 40, true),
            ("Whenever an opponent casts a spell, you may put a +1/+1 counter on CARDNAME.", 60, 40, true)] {
            let mut v = with_library(view(0, StepKind::Main1, vec![]), library);
            v.players[0].life = life;
            let mut bot = SimpleAi::default();
            bot.observe(v);
            assert_eq!(confirm(&mut bot, "card-1", title), expected, "{title} library {library} life {life}");
        }
    }

    #[test]
    fn paying_life_for_a_sources_own_draw_is_accepted_with_a_library_and_life_to_spare() {
        let ring = CardDto { id: "card-30".into(), controller_id: "player-0".into(), owner_id: "player-0".into(), types: vec!["Enchantment".into()],
            text: "Whenever you choose a creature as your Ring-bearer, you may pay 2 life. If you do, draw a card.".into(), ..Default::default() };
        for (library, life, expected) in [(60, 40, true), (3, 40, false), (60, 10, false)] {
            let mut v = with_library(view(0, StepKind::Upkeep, vec![(ZoneKind::Battlefield, 0, vec![ring.clone()])]), library);
            v.players[0].life = life;
            let mut bot = SimpleAi::default();
            bot.observe(v);
            assert_eq!(confirm(&mut bot, "card-30", "Pay 2 {LIFE} for Call of the Ring?"), expected, "library {library} life {life}");
        }
    }

    #[test]
    fn a_shock_land_enters_untapped_when_the_mana_is_needed_or_life_is_comfortable() {
        let shock = land("card-20", 0, "Mountain");
        let mut spell = spell_card("card-21", "{2}{R}", 3, "Draw two cards.");
        spell.types = vec!["Sorcery".into()];
        for (own_turn, hand, life, expected) in [(0, true, 30, true), (0, true, 15, true), (0, false, 15, false), (1, true, 15, false), (0, false, 40, true)] {
            let hand = if hand { vec![spell.clone()] } else { vec![] };
            let battlefield = vec![shock.clone(), land("card-22", 0, "Mountain"), land("card-23", 0, "Mountain")];
            let mut v = view(own_turn, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, battlefield), (ZoneKind::Hand, 0, hand)]);
            v.players[0].life = life;
            let mut bot = SimpleAi::default();
            bot.observe(v);
            assert_eq!(confirm(&mut bot, "card-20", "Pay 2 {LIFE} for Mountain?"), expected, "turn {own_turn} life {life}");
        }
    }

    #[test]
    fn an_optional_discard_draw_needs_a_card_worth_less() {
        const LOOT: &str = "When CARDNAME enters, you may discard a card. If you do, draw a card.";
        let lands: Vec<_> = (0..6).map(|i| land(&format!("card-3{i}"), 0, "Island")).collect();
        let extra_land = land("card-40", 0, "Island");
        let good_spell = spell_card("card-41", "{2}{U}", 3, "Counter target spell.");
        for (hand, expected) in [(vec![extra_land], true), (vec![good_spell], false)] {
            let mut bot = SimpleAi::default();
            bot.observe(with_library(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, lands.clone()), (ZoneKind::Hand, 0, hand)]), 60));
            assert_eq!(confirm(&mut bot, "card-1", LOOT), expected);
        }
    }

    #[test]
    fn a_pile_split_is_balanced_and_the_caster_takes_the_better_pile() {
        let big = creature("card-50", 1, (6, 6), 6, "");
        let mut cards = vec![big.clone()];
        cards.extend((51..55).map(|i| land(&format!("card-{i}"), 1, "Swamp")));
        let one = SimpleAi::pile_one_cards(&cards, 1, 4);
        assert!((1..=4).contains(&one.len()), "never 0 and 5: {one:?}");
        let order: Vec<_> = cards.iter().filter(|c| one.contains(&c.id)).chain(cards.iter().filter(|c| !one.contains(&c.id))).cloned().collect();
        let pick = SimpleAi::better_pile_card(&order, one.len()).unwrap();
        assert_eq!(pick, "card-50", "the caster names the pile with the big creature");
    }

    /// The target prompt a cast would reach: the engine offers `ids` (it leaves out what the spell
    /// cannot target), and a voluntary cast is cancellable.
    fn prompt_for(bot: &mut SimpleAi, spell: &CardDto, intent: &str, ids: &[&str]) -> Value {
        bot.pending_voluntary_cast = Some(spell.id.clone());
        let candidates: Vec<_> = ids.iter().map(|id| json!({"id":id,"kind":"card"})).collect();
        let prompt: AgentPrompt = serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","sourceCard":spell,"sourceAbilityText":spell.text,
            "input":{"type":"chooseBoardTargets","presentation":{"title":"Target","targets":[]},"candidates":candidates,
            "hostile":true,"intent":intent,"minTargets":1,"maxTargets":1,"chosenTargets":0,"cancellable":true}})).unwrap();
        serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone()
    }

    /// DEV-092: one rule decides both "is there a worthwhile target" before casting and "which
    /// target" at the prompt, for blink and bounce spells alike. Each position is put to the cast
    /// check and to the prompt the engine would then raise; the two must agree, and agree with the
    /// expected play. Shroud, protection from the spell's colour, an opponent's hexproof and a
    /// creature another seat is already answering are not worth a cast.
    #[test]
    fn the_cast_check_and_the_target_prompt_cannot_disagree() {
        const BLINK: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
        const BOUNCE: &str = "Return target creature to its owner's hand.";
        const ENTRY: &str = "When this creature enters, draw a card.";
        let with = |mut card: CardDto, keyword: &str| { card.keywords.push(keyword.into()); card };
        let mut token = creature("card-3", 0, (1, 1), 0, ENTRY);
        token.identity.is_token = true;
        let exile_elsewhere = seat_item("stack-1", 2, "Exile target creature.", "card-5", "card", "exile");
        // (label, spell colour, spell text, intent, our creatures, their creatures, stack, offered by the engine, expected)
        let cases: Vec<(&str, &str, &str, &str, Vec<CardDto>, Vec<CardDto>, Vec<StackObjectDto>, Vec<&str>, bool)> = vec![
            ("blink an entry creature", "W", BLINK, "exile", vec![creature("card-1", 0, (2, 2), 3, ENTRY), token.clone()], vec![], vec![], vec!["card-1", "card-3"], true),
            ("blink: the entry creature has shroud", "W", BLINK, "exile", vec![with(creature("card-1", 0, (2, 2), 3, ENTRY), "Shroud"), token.clone()], vec![], vec![], vec!["card-3"], false),
            ("blink: protection from the spell's colour", "W", BLINK, "exile", vec![with(creature("card-1", 0, (2, 2), 3, ENTRY), "Protection:White:Card.White:white"), token.clone()], vec![], vec![], vec!["card-3"], false),
            ("blink: protection from another colour", "W", BLINK, "exile", vec![with(creature("card-1", 0, (2, 2), 3, ENTRY), "Protection:Red:Card.Red:red"), token.clone()], vec![], vec![], vec!["card-1", "card-3"], true),
            ("blink: protection from the same colour, spell of another colour", "U", BLINK, "exile", vec![with(creature("card-1", 0, (2, 2), 3, ENTRY), "Protection:White:Card.White:white"), token.clone()], vec![], vec![], vec!["card-1", "card-3"], true),
            ("bounce an opponent's creature", "U", BOUNCE, "bounce", vec![], vec![creature("card-5", 1, (2, 2), 3, "")], vec![], vec!["card-5"], true),
            ("bounce: already being exiled by another seat", "U", BOUNCE, "bounce", vec![], vec![creature("card-5", 1, (2, 2), 3, "")], vec![exile_elsewhere], vec!["card-5"], false),
            ("bounce: opponent's creature has hexproof", "U", BOUNCE, "bounce", vec![], vec![with(creature("card-5", 1, (2, 2), 3, ""), "Hexproof")], vec![], vec![], false),
            ("bounce: only our own creature, not threatened", "U", BOUNCE, "bounce", vec![creature("card-1", 0, (2, 2), 3, "")], vec![], vec![], vec!["card-1"], false),
        ];
        for (label, colour, text, intent, ours, theirs, stack, offered, expected) in cases {
            let mut battlefield: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 20 + i), 0, "Island")).collect();
            battlefield.extend(ours.into_iter().map(|mut card| { card.zone = Some(ZoneKind::Battlefield); card }));
            let mut spell = spell_card("card-9", "{U}", 1, text);
            spell.color = colour.into();
            let mut zones = vec![(ZoneKind::Battlefield, 0, battlefield), (ZoneKind::Hand, 0, vec![spell.clone()])];
            if !theirs.is_empty() { zones.push((ZoneKind::Battlefield, 1, theirs.into_iter().map(|mut card| { card.zone = Some(ZoneKind::Battlefield); card }).collect())); }
            let mut view = view(0, StepKind::Main1, zones);
            view.stack = stack;
            let mut bot = SimpleAi::default();
            bot.observe(view.clone());
            let cast = cast_choice(&mut bot, "card-9") == "act";
            let mut bot = SimpleAi::default();
            bot.observe(view);
            let picked = if offered.is_empty() { false } else { prompt_for(&mut bot, &spell, intent, &offered)["type"] == "boardTargets" };
            assert_eq!(cast, picked, "{label}: cast check and prompt disagree");
            assert_eq!(cast, expected, "{label}");
        }
    }

    /// An "exile target card from your graveyard" spell is no removal: its own card is the target.
    #[test]
    fn exiling_a_card_from_our_graveyard_is_not_cancelled_as_a_removal() {
        let mut spell = spell_card("card-9", "{U}", 1, "Exile target card that's an instant or sorcery from your graveyard. Copy it.");
        spell.color = "U".into();
        let mut dead = spell_card("card-40", "{R}", 1, "Deal 3 damage to any target.");
        dead.zone = Some(ZoneKind::Graveyard);
        let lands: Vec<_> = (0..3).map(|i| land(&format!("card-{}", 20 + i), 0, "Island")).collect();
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, lands), (ZoneKind::Hand, 0, vec![spell.clone()]), (ZoneKind::Graveyard, 0, vec![dead])]));
        assert_eq!(cast_choice(&mut bot, "card-9"), "act");
        assert_eq!(prompt_for(&mut bot, &spell, "exile", &["card-40"])["type"], "boardTargets");
    }

    /// DEV-090: "that player controls" limits the pick to the player our combat reached, though the
    /// engine lists every creature. With none of theirs offered a voluntary pick is declined.
    #[test]
    fn a_target_limited_to_the_player_we_hit_is_taken_from_that_player() {
        const GOAD: &str = "Goad target creature that player controls.";
        let mut attacker = creature("card-1", 0, (2, 2), 3, "");
        attacker.is_attacking = true;
        attacker.attack_target_id = Some("player-1".into());
        let zones = vec![
            (ZoneKind::Battlefield, 0, vec![attacker]),
            (ZoneKind::Battlefield, 1, vec![creature("card-10", 1, (1, 1), 1, "")]),
            (ZoneKind::Battlefield, 2, vec![creature("card-20", 2, (8, 8), 8, "Flying")]),
        ];
        let mut bot = SimpleAi::default();
        bot.observe(view(0, StepKind::CombatDamage, zones));
        let out = targets(&mut bot, "card-9", GOAD, "hostile", &["card-20", "card-10", "card-1"], 1, false);
        assert_eq!(out["chosen"][0]["id"], "card-10", "the better creature belongs to a player we did not hit");
        let out = targets(&mut bot, "card-9", GOAD, "hostile", &["card-20"], 1, true);
        assert_eq!(out["type"], "cancel", "nothing legal offered and the pick is optional");
        let out = targets(&mut bot, "card-9", GOAD, "hostile", &["card-20"], 1, false);
        assert_eq!(out["chosen"][0]["id"], "card-20", "a required pick still answers");
        // A text without the limit keeps ranking every candidate.
        let out = targets(&mut bot, "card-9", "Goad target creature.", "hostile", &["card-20", "card-10"], 1, false);
        assert_eq!(out["chosen"][0]["id"], "card-20");
    }

    fn land_with(id: &str, text: &str) -> CardDto {
        CardDto { id: id.into(), controller_id: "player-0".into(), owner_id: "player-0".into(), types: vec!["Land".into()], text: text.into(), ..Default::default() }
    }

    /// DEV-091: a cost is not started when no tap order pays it. One land making green, white or
    /// blue cannot pay both the green and the blue pip, however many colours the lands add up to.
    #[test]
    fn a_four_colour_spell_is_cast_only_when_each_colour_has_its_own_source() {
        let spara = land_with("card-30", "{T}: Add {W}.\n{T}: Add {U}.\n{T}: Add {G}.");
        let shrine = land_with("card-31", "{T}: Add {W}.\n{T}: Add {B}.");
        let ring = land_with("card-32", "{T}: Add {C}{C}.");
        let plains = land_with("card-33", "{T}: Add {W}.");
        let island = land_with("card-34", "{T}: Add {U}.");
        for (extra, expected) in [(None, "pass"), (Some(island), "act")] {
            let mut lands = vec![spara.clone(), shrine.clone(), ring.clone(), plains.clone()];
            lands.extend(extra);
            let hand = vec![spell_card("card-9", "{G}{W}{U}{B}", 4, "Draw a card.")];
            let mut bot = SimpleAi::default();
            bot.observe(view(0, StepKind::Main1, vec![(ZoneKind::Battlefield, 0, lands), (ZoneKind::Hand, 0, hand)]));
            assert_eq!(cast_choice(&mut bot, "card-9"), expected);
        }
    }
}

#[cfg(test)]
mod skill_puzzles;

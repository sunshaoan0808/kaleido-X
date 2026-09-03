//! Pockets & Wardrobe — per-character, per-session inventory.
//!
//! Port of Front Porch AI `lib/services/chat/pockets.dart` (AGPL-3.0, reimplemented).
//! Keeps what a character is **wearing**, **carrying**, and **set aside nearby** —
//! strictly session-scoped, like Journal cards. Nothing crosses conversations.
//!
//! Design (Front Porch `docs/design/pockets-and-preferences.md` Part 1):
//! - Clothing and possessions have **opposite expiry**: clothes expire at the
//!   next story morning (people dress fresh); possessions never expire.
//! - Caps protect the prompt: `kMaxWorn/kMaxCarrying = 8`, `kMaxSetAside = 16`.
//! - `setAside` entries are additive (`set_aside` key omitted when empty) — an
//!   untouched record stays byte-identical to one written before this existed.
//! - The **ONE applier** `apply_pocket_ops` is the only mutation path.
//!
//! No I/O, no LLM, no async — pure data + applier, fully unit-testable.

use serde::{Deserialize, Serialize};

/// Longest item name / condition kept.
pub const K_MAX_ITEM_NAME_CHARS: usize = 60;
pub const K_MAX_ITEM_STATE_CHARS: usize = 60;

/// Max items per list.
pub const K_MAX_WORN: usize = 8;
pub const K_MAX_CARRYING: usize = 8;
/// Max set-aside entries (one full outfit + one full carrying).
pub const K_MAX_SET_ASIDE: usize = K_MAX_WORN + K_MAX_CARRYING;

// ── PocketItem ───────────────────────────────────────────────────────────────

/// One thing, worn or carried.
///
/// `state` is free-text condition ("half-eaten", "rain-soaked").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PocketItem {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
}

impl PocketItem {
    /// Trim, whitespace-collapse, length-cap.
    pub fn clean(name: impl AsRef<str>, state: impl AsRef<str>) -> Self {
        Self {
            name: tidy(name.as_ref(), K_MAX_ITEM_NAME_CHARS),
            state: tidy(state.as_ref(), K_MAX_ITEM_STATE_CHARS),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }

    /// `"iron sword (notched)"` — prompt + UI display.
    pub fn display(&self) -> String {
        if self.state.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.state)
        }
    }

    pub fn with_state(&self, s: impl AsRef<str>) -> Self {
        Self {
            name: self.name.clone(),
            state: tidy(s.as_ref(), K_MAX_ITEM_STATE_CHARS),
        }
    }

    /// Parse `"iron sword (notched)"` → name + state.
    /// Round-trips via `display()` exactly.
    pub fn parse_display(raw: &str) -> Self {
        let s = raw.trim();
        if s.ends_with(')') {
            if let Some(open) = s.rfind('(') {
                if open > 0 {
                    let name = s[..open].trim();
                    let state = s[open + 1..s.len() - 1].trim();
                    if !name.is_empty() && !state.is_empty() {
                        return Self::clean(name, state);
                    }
                }
            }
        }
        Self::clean(s, "")
    }

    pub fn from_json(raw: &serde_json::Value) -> Option<Self> {
        match raw {
            serde_json::Value::String(s) => {
                let it = Self::clean(s.as_str(), "");
                if it.is_empty() { None } else { Some(it) }
            }
            serde_json::Value::Object(map) => {
                let name = map.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let state = map.get("state").and_then(|v| v.as_str()).unwrap_or("");
                let it = Self::clean(name, state);
                if it.is_empty() { None } else { Some(it) }
            }
            _ => None,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), serde_json::Value::String(self.name.clone()));
        if !self.state.is_empty() {
            m.insert("state".into(), serde_json::Value::String(self.state.clone()));
        }
        serde_json::Value::Object(m)
    }
}

// ── SetAsideItem ───────────────────────────────────────────────────────────

/// One thing set aside — still hers, still in the scene, just not on body.
/// `clothing` controls expiry asymmetry; `day` is story day parked (0 = no clock).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetAsideItem {
    pub item: PocketItem,
    pub clothing: bool,
    #[serde(default)]
    pub day: u32,
}

impl SetAsideItem {
    pub fn new(item: PocketItem, clothing: bool, day: u32) -> Self {
        Self { item, clothing, day }
    }

    pub fn with_item(&self, it: PocketItem) -> Self {
        Self { item: it, clothing: self.clothing, day: self.day }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut m = match self.item.to_json() {
            serde_json::Value::Object(mm) => mm,
            _ => serde_json::Map::new(),
        };
        m.insert("clothing".into(), serde_json::Value::Bool(self.clothing));
        if self.day > 0 {
            m.insert("day".into(), serde_json::Value::Number(self.day.into()));
        }
        serde_json::Value::Object(m)
    }

    pub fn from_json(raw: &serde_json::Value) -> Option<Self> {
        let map = raw.as_object()?;
        let item = PocketItem::from_json(raw)?;
        let clothing = map.get("clothing").and_then(|v| v.as_bool()).unwrap_or(false);
        let day = map.get("day").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        Some(Self { item, clothing, day })
    }
}

// ── PocketSection / PocketOpKind ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PocketSection {
    Worn,
    Carrying,
    SetAside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PocketOpKind {
    Wear,
    Remove,
    Pickup,
    Drop,
    Give,
    Setdown,
    Update,
    Transform,
}

impl PocketOpKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "wear" => Some(Self::Wear),
            "remove" => Some(Self::Remove),
            "pickup" => Some(Self::Pickup),
            "drop" => Some(Self::Drop),
            "give" => Some(Self::Give),
            "setdown" => Some(Self::Setdown),
            "update" => Some(Self::Update),
            "transform" => Some(Self::Transform),
            // forgiving synonyms (Front Porch)
            "put_on" | "puton" | "equip" => Some(Self::Wear),
            "take_off" | "takeoff" | "unequip" => Some(Self::Remove),
            "take" | "pick_up" | "get" => Some(Self::Pickup),
            "discard" | "lose" => Some(Self::Drop),
            "hand" => Some(Self::Give),
            "set_down" | "put_down" | "putdown" | "set_aside" | "setaside" | "place" | "stow" => Some(Self::Setdown),
            "become" | "becomes" => Some(Self::Transform),
            _ => None,
        }
    }
}

// ── PocketOpReport / PocketEvent ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PocketOpReport {
    pub kind: PocketOpKind,
    pub item: String,
    pub to: String,
    pub state: String,
    pub where_: String,
}

impl PocketOpReport {
    pub fn from_json(raw: &serde_json::Value) -> Option<Self> {
        let map = raw.as_object()?;
        let kind = PocketOpKind::parse(map.get("op")?.as_str()?)?;
        let item = tidy(map.get("item").and_then(|v| v.as_str()).unwrap_or(""), K_MAX_ITEM_NAME_CHARS);
        if item.is_empty() { return None; }
        Some(Self {
            kind,
            item,
            to: tidy(map.get("to").and_then(|v| v.as_str()).unwrap_or(""), K_MAX_ITEM_NAME_CHARS),
            state: tidy(map.get("state").and_then(|v| v.as_str()).unwrap_or(""), K_MAX_ITEM_STATE_CHARS),
            where_: tidy(map.get("where").and_then(|v| v.as_str()).unwrap_or(""), K_MAX_ITEM_STATE_CHARS),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PocketEvent {
    pub kind: PocketOpKind,
    pub item: String,
    pub to: String,
    pub where_: String,
    pub clothing: bool,
    pub bulk: bool,
}

// ── Pockets ────────────────────────────────────────────────────────────────

/// One character's pockets in one chat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pockets {
    #[serde(default)]
    pub worn: Vec<PocketItem>,
    #[serde(default)]
    pub carrying: Vec<PocketItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set_aside: Vec<SetAsideItem>,
}

impl Pockets {
    pub fn is_empty(&self) -> bool {
        self.worn.is_empty() && self.carrying.is_empty() && self.set_aside.is_empty()
    }

    /// View of set-aside still standing on story `day`.
    pub fn set_aside_on(&self, day: u32) -> Vec<&SetAsideItem> {
        self.set_aside.iter().filter(|e| !e.clothing || e.day == 0 || e.day >= day).collect()
    }

    /// Expire clothing entries whose `day < current day`.
    pub fn expire_set_aside(&mut self, day: u32) {
        if day == 0 { return; }
        self.set_aside.retain(|e| !(e.clothing && e.day > 0 && e.day < day));
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("worn".into(), serde_json::Value::Array(self.worn.iter().map(|i| i.to_json()).collect()));
        m.insert("carrying".into(), serde_json::Value::Array(self.carrying.iter().map(|i| i.to_json()).collect()));
        if !self.set_aside.is_empty() {
            m.insert("set_aside".into(), serde_json::Value::Array(self.set_aside.iter().map(|e| e.to_json()).collect()));
        }
        serde_json::Value::Object(m)
    }

    /// `to_json` as story sees it on `day` (expired clothing filtered).
    pub fn to_json_on(&self, day: u32) -> serde_json::Value {
        let filtered: Vec<serde_json::Value> = self.set_aside_on(day).into_iter().map(|e| e.to_json()).collect();
        let mut m = serde_json::Map::new();
        m.insert("worn".into(), serde_json::Value::Array(self.worn.iter().map(|i| i.to_json()).collect()));
        m.insert("carrying".into(), serde_json::Value::Array(self.carrying.iter().map(|i| i.to_json()).collect()));
        if !filtered.is_empty() {
            m.insert("set_aside".into(), serde_json::Value::Array(filtered));
        }
        serde_json::Value::Object(m)
    }

    pub fn from_json(raw: &serde_json::Value) -> Self {
        let map = match raw.as_object() { Some(m) => m, None => return Self::default() };
        let parse_list = |key: &str, cap: usize| -> Vec<PocketItem> {
            match map.get(key).and_then(|v| v.as_array()) {
                Some(arr) => arr.iter().take(cap).filter_map(PocketItem::from_json).collect(),
                None => vec![],
            }
        };
        let set_aside = match map.get("set_aside").and_then(|v| v.as_array()) {
            Some(arr) => arr.iter().take(K_MAX_SET_ASIDE).filter_map(SetAsideItem::from_json).collect(),
            None => vec![],
        };
        Self {
            worn: parse_list("worn", K_MAX_WORN),
            carrying: parse_list("carrying", K_MAX_CARRYING),
            set_aside,
        }
    }

    pub fn worn_display(&self) -> Vec<String> { self.worn.iter().map(|i| i.display()).collect() }
    pub fn carrying_display(&self) -> Vec<String> { self.carrying.iter().map(|i| i.display()).collect() }

    /// Build card `frontPorchExtensions.inventory` map from chip text lists.
    pub fn card_json_from(worn: &[String], carrying: &[String]) -> serde_json::Value {
        let worn_items: Vec<PocketItem> = worn.iter().map(|s| PocketItem::parse_display(s)).filter(|i| !i.is_empty() && !is_empty_wardrobe_ref(&i.name)).collect();
        let carrying_items: Vec<PocketItem> = carrying.iter().map(|s| PocketItem::parse_display(s)).filter(|i| !i.is_empty()).collect();
        let p = Self::from_json(&serde_json::json!({
            "worn": worn_items.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
            "carrying": carrying_items.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        }));
        if p.is_empty() { serde_json::json!({}) } else { p.to_json() }
    }

    /// Prompt injection block for LLM (wardrobeContext).
    pub fn wardrobe_context(&self, char_name: &str, day: u32) -> String {
        let mut out = String::new();
        if !self.worn.is_empty() {
            out.push_str(&format!("Currently wearing: {}\n", self.worn.iter().map(|i| i.display()).collect::<Vec<_>>().join(", ")));
        }
        if !self.carrying.is_empty() {
            out.push_str(&format!("Currently carrying: {}\n", self.carrying.iter().map(|i| i.display()).collect::<Vec<_>>().join(", ")));
        }
        let aside = self.set_aside_on(day);
        if !aside.is_empty() {
            out.push_str(&format!("Set aside nearby (still {}'s): {}\n", char_name, aside.iter().map(|e| e.item.display()).collect::<Vec<_>>().join(", ")));
        }
        if out.is_empty() { String::new() } else { format!("What {char_name} is wearing and carrying:\n{out}") }
    }
}

// ── helpers: token / generic refs / sameItem ───────────────────────────────

const FILLER: &[&str] = &["a", "an", "the", "her", "his", "their", "my", "your", "of"];

fn norm(s: &str) -> String {
    s.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ').collect()
}

fn content_tokens(s: &str) -> std::collections::HashSet<String> {
    norm(s).split_whitespace().filter(|t| !FILLER.contains(t)).map(|t| t.to_string()).collect()
}

pub fn item_name_tokens(s: &str) -> std::collections::HashSet<String> {
    content_tokens(s).into_iter().filter(|t| t.len() >= 3).collect()
}

pub fn is_generic_clothing_ref(raw: &str) -> bool {
    let toks = content_tokens(raw);
    if toks.is_empty() { return false; }
    const GENERIC: &[&str] = &["clothes", "clothing", "outfit", "garments", "everything", "all"];
    toks.iter().all(|t| GENERIC.contains(&t.as_str()))
}

pub fn is_empty_wardrobe_ref(raw: &str) -> bool {
    let toks = content_tokens(raw);
    if toks.is_empty() { return false; }
    const EMPTY: &[&str] = &["nothing", "none", "nude", "naked", "unclothed", "undressed", "bare", "empty"];
    toks.iter().all(|t| EMPTY.contains(&t.as_str()))
}

pub fn is_generic_things_ref(raw: &str) -> bool {
    let toks = content_tokens(raw);
    if toks.is_empty() { return false; }
    const GENERIC: &[&str] = &["things", "belongings", "stuff", "everything", "all"];
    toks.iter().all(|t| GENERIC.contains(&t.as_str()))
}

pub fn same_item(a: &str, b: &str) -> bool {
    let an = norm(a);
    let bn = norm(b);
    if an.is_empty() || bn.is_empty() { return false; }
    if an == bn { return true; }
    let at = content_tokens(a);
    let bt = content_tokens(b);
    if at.is_empty() || bt.is_empty() { return false; }
    at.is_superset(&bt) || bt.is_superset(&at)
}

pub fn resolve_recipient(to: &str, names: &[String]) -> Option<String> {
    let t = to.trim().to_ascii_lowercase();
    if t.is_empty() || names.is_empty() { return None; }
    for n in names {
        if n.trim().to_ascii_lowercase() == t { return Some(n.clone()); }
    }
    // first-name unique pass
    let mut firsts: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for n in names {
        let f = n.trim().split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if !f.is_empty() { firsts.entry(f).or_default().push(n.clone()); }
    }
    if let Some(hit) = firsts.get(&t) { if hit.len() == 1 { return Some(hit[0].clone()); } }
    None
}

fn tidy(s: &str, cap: usize) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = t.trim();
    if trimmed.chars().count() <= cap { trimmed.to_string() } else { trimmed.chars().take(cap).collect::<String>().trim_end().to_string() }
}

// ── THE applier ────────────────────────────────────────────────────────────

/// Apply ops to `p` in place; return receipt lines.
/// `on_transfer` fires for `give` with a resolved recipient.
/// `day` stamps new set-aside entries and expires yesterday's clothing.
/// `events` collects deterministic `PocketEvent` per applied change.
fn park_item(p: &mut Pockets, item: PocketItem, clothing: bool, day: u32) {
    p.set_aside.push(SetAsideItem::new(item, clothing, day));
    while p.set_aside.len() > K_MAX_SET_ASIDE {
        if let Some(idx) = p.set_aside.iter().position(|e| e.clothing) { p.set_aside.remove(idx); } else { p.set_aside.remove(0); }
    }
}

fn do_undress_all(p: &mut Pockets, day: u32, receipts: &mut Vec<String>, ev_out: &mut Vec<PocketEvent>) {
    for it in std::mem::take(&mut p.worn) {
        receipts.push(format!("took off: {}", it.name));
        ev_out.push(PocketEvent { kind: PocketOpKind::Remove, item: it.name.clone(), to: String::new(), where_: String::new(), clothing: true, bulk: true });
        park_item(p, it, true, day);
    }
    for it in std::mem::take(&mut p.carrying) {
        receipts.push(format!("set aside: {}", it.name));
        ev_out.push(PocketEvent { kind: PocketOpKind::Remove, item: it.name.clone(), to: String::new(), where_: String::new(), clothing: false, bulk: true });
        park_item(p, it, false, day);
    }
}

fn find_item(list: &[PocketItem], name: &str) -> Option<usize> { list.iter().position(|i| same_item(&i.name, name)) }
fn find_aside(set_aside: &[SetAsideItem], name: &str) -> Option<usize> { set_aside.iter().position(|e| same_item(&e.item.name, name)) }
fn cap_to(list: &mut Vec<PocketItem>, max: usize) { while list.len() > max { list.remove(0); } }

pub fn apply_pocket_ops(
    p: &mut Pockets,
    ops: &[PocketOpReport],
    mut on_transfer: Option<&mut dyn FnMut(String, PocketItem)>,
    day: u32,
    events: Option<&mut Vec<PocketEvent>>,
) -> Vec<String> {
    let mut receipts: Vec<String> = vec![];
    let mut ev_out: Vec<PocketEvent> = vec![];
    p.expire_set_aside(day);

    for op in ops {
        match op.kind {
            PocketOpKind::Wear => {
                if is_empty_wardrobe_ref(&op.item) {
                    if p.worn.is_empty() && p.carrying.is_empty() { continue; }
                    do_undress_all(p, day, &mut receipts, &mut ev_out);
                    continue;
                }
                if is_generic_clothing_ref(&op.item) {
                    let back: Vec<SetAsideItem> = p.set_aside.iter().filter(|e| e.clothing).cloned().collect();
                    for e in back {
                        if let Some(idx) = p.set_aside.iter().position(|x| x == &e) { p.set_aside.remove(idx); }
                        receipts.push(format!("put on: {}", e.item.name));
                        ev_out.push(PocketEvent { kind: PocketOpKind::Wear, item: e.item.name.clone(), to: String::new(), where_: String::new(), clothing: true, bulk: false });
                        p.worn.push(e.item);
                    }
                    cap_to(&mut p.worn, K_MAX_WORN);
                    continue;
                }
                if let Some(idx) = find_item(&p.worn, &op.item) {
                    if !op.state.is_empty() && p.worn[idx].state != op.state {
                        p.worn[idx] = p.worn[idx].with_state(&op.state);
                        receipts.push(format!("{}: {}", op.item, op.state));
                    }
                    continue;
                }
                let c = find_item(&p.carrying, &op.item);
                let s = if c.is_none() { find_aside(&p.set_aside, &op.item) } else { None };
                let item = if let Some(ci) = c { p.carrying.remove(ci) } else if let Some(si) = s { p.set_aside.remove(si).item } else { PocketItem::clean(&op.item, &op.state) };
                let to_push = if op.state.is_empty() { item.clone() } else { item.with_state(&op.state) };
                let name = to_push.name.clone();
                p.worn.push(to_push);
                cap_to(&mut p.worn, K_MAX_WORN);
                receipts.push(format!("put on: {}", op.item));
                ev_out.push(PocketEvent { kind: PocketOpKind::Wear, item: name, to: String::new(), where_: String::new(), clothing: true, bulk: false });
            }
            PocketOpKind::Remove => {
                if is_generic_clothing_ref(&op.item) {
                    do_undress_all(p, day, &mut receipts, &mut ev_out);
                    continue;
                }
                let w = find_item(&p.worn, &op.item);
                if w.is_none() { continue; }
                let removed = p.worn.remove(w.unwrap());
                let name = removed.name.clone();
                park_item(p, removed, true, day);
                receipts.push(format!("took off: {}", op.item));
                ev_out.push(PocketEvent { kind: PocketOpKind::Remove, item: name, to: String::new(), where_: String::new(), clothing: true, bulk: false });
            }
            PocketOpKind::Setdown => {
                if is_generic_things_ref(&op.item) {
                    for it in std::mem::take(&mut p.carrying) {
                        let name = it.name.clone();
                        park_item(p, it, false, day);
                        receipts.push(format!("set aside: {}", name));
                        ev_out.push(PocketEvent { kind: PocketOpKind::Setdown, item: name, to: String::new(), where_: op.where_.clone(), clothing: false, bulk: false });
                    }
                    continue;
                }
                if let Some(sc) = find_item(&p.carrying, &op.item) {
                    let down = p.carrying.remove(sc);
                    let name = down.name.clone();
                    park_item(p, down, false, day);
                    receipts.push(format!("set aside: {}", op.item));
                    ev_out.push(PocketEvent { kind: PocketOpKind::Setdown, item: name, to: String::new(), where_: op.where_.clone(), clothing: false, bulk: false });
                    continue;
                }
                if let Some(sw) = find_item(&p.worn, &op.item) {
                    let down = p.worn.remove(sw);
                    let name = down.name.clone();
                    park_item(p, down, true, day);
                    receipts.push(format!("set aside: {}", op.item));
                    ev_out.push(PocketEvent { kind: PocketOpKind::Setdown, item: name, to: String::new(), where_: op.where_.clone(), clothing: true, bulk: false });
                }
            }
            PocketOpKind::Pickup => {
                if find_item(&p.carrying, &op.item).is_some() || find_item(&p.worn, &op.item).is_some() { continue; }
                let sa = find_aside(&p.set_aside, &op.item);
                let got = if let Some(si) = sa { p.set_aside.remove(si).item } else { PocketItem::clean(&op.item, &op.state) };
                let to_push = if op.state.is_empty() { got.clone() } else { got.with_state(&op.state) };
                let name = to_push.name.clone();
                p.carrying.push(to_push);
                cap_to(&mut p.carrying, K_MAX_CARRYING);
                receipts.push(format!("picked up: {}", op.item));
                ev_out.push(PocketEvent { kind: PocketOpKind::Pickup, item: name, to: String::new(), where_: String::new(), clothing: false, bulk: false });
            }
            PocketOpKind::Drop | PocketOpKind::Give => {
                let c = find_item(&p.carrying, &op.item);
                let w = if c.is_none() { find_item(&p.worn, &op.item) } else { None };
                let sa = if c.is_none() && w.is_none() { find_aside(&p.set_aside, &op.item) } else { None };
                if c.is_none() && w.is_none() && sa.is_none() { continue; }
                let taken = if let Some(ci) = c { p.carrying.remove(ci) } else if let Some(wi) = w { p.worn.remove(wi) } else { p.set_aside.remove(sa.unwrap()).item };
                let is_worn = w.is_some();
                let taken_clone = taken.clone();
                if op.kind == PocketOpKind::Give && !op.to.is_empty() {
                    if let Some(cb) = on_transfer.as_mut() { cb(op.to.clone(), taken_clone); }
                }
                if op.kind == PocketOpKind::Give && !op.to.is_empty() {
                    receipts.push(format!("gave {} to {}", op.item, op.to));
                } else {
                    receipts.push(format!("dropped: {}", op.item));
                }
                ev_out.push(PocketEvent { kind: op.kind, item: taken.name, to: op.to.clone(), where_: op.where_.clone(), clothing: is_worn, bulk: false });
            }
            PocketOpKind::Update => {
                if op.state.is_empty() { continue; }
                let mut touched = false;
                if let Some(idx) = find_item(&p.worn, &op.item) {
                    if p.worn[idx].state != op.state {
                        p.worn[idx] = p.worn[idx].with_state(&op.state);
                        receipts.push(format!("{}: {}", op.item, op.state));
                    }
                    touched = true;
                } else if let Some(idx) = find_item(&p.carrying, &op.item) {
                    if p.carrying[idx].state != op.state {
                        p.carrying[idx] = p.carrying[idx].with_state(&op.state);
                        receipts.push(format!("{}: {}", op.item, op.state));
                    }
                    touched = true;
                }
                if touched { continue; }
                if let Some(u) = find_aside(&p.set_aside, &op.item) {
                    if p.set_aside[u].item.state != op.state {
                        let it = p.set_aside[u].item.with_state(&op.state);
                        p.set_aside[u] = p.set_aside[u].with_item(it);
                        receipts.push(format!("{}: {}", op.item, op.state));
                    }
                }
            }
            PocketOpKind::Transform => {
                if op.state.is_empty() { continue; }
                let mut morphed = false;
                if let Some(idx) = find_item(&p.worn, &op.item) {
                    p.worn[idx] = PocketItem::clean(&op.state, "");
                    receipts.push(format!("{} → {}", op.item, op.state));
                    morphed = true;
                } else if let Some(idx) = find_item(&p.carrying, &op.item) {
                    p.carrying[idx] = PocketItem::clean(&op.state, "");
                    receipts.push(format!("{} → {}", op.item, op.state));
                    morphed = true;
                }
                if morphed { continue; }
                if let Some(t) = find_aside(&p.set_aside, &op.item) {
                    p.set_aside[t] = p.set_aside[t].with_item(PocketItem::clean(&op.state, ""));
                    receipts.push(format!("{} → {}", op.item, op.state));
                }
            }
        }
    }
    if let Some(out) = events { out.extend(ev_out); }
    receipts
}

// ── tests ──────────────────────────────────────────────────────────────────


/// Item-memory card drafts from applied pocket events (setdown/give/drop + outfit change).
/// Port of Front Porch AI `pocket_journal_cards.dart` (AGPL-3.0, reimplemented).
/// Deterministic: undress/dress alone silent; wear+remove(clothing) in same turn = one change card.
pub fn item_cards_from(events: &[PocketEvent]) -> Vec<(String, String)> {
    let mut drafts: Vec<(String, String)> = vec![];
    let placed = |w: &str| if w.is_empty() { String::new() } else { format!(" — {w}") };
    for e in events {
        match e.kind {
            PocketOpKind::Setdown => drafts.push((e.item.clone(), format!("I set my {} down{}.", e.item, placed(&e.where_)))),
            PocketOpKind::Give => drafts.push((e.item.clone(), if e.to.is_empty() { format!("I handed my {} over.", e.item) } else { format!("I gave my {} to {}.", e.item, e.to) })),
            PocketOpKind::Drop => drafts.push((e.item.clone(), format!("I parted with my {}{}.", e.item, placed(&e.where_)))),
            _ => {}
        }
    }
    let wears: Vec<String> = events.iter().filter(|e| e.kind == PocketOpKind::Wear).map(|e| e.item.clone()).collect();
    let removes: Vec<String> = events.iter().filter(|e| e.kind == PocketOpKind::Remove && e.clothing).map(|e| e.item.clone()).collect();
    if !wears.is_empty() && !removes.is_empty() {
        let short = |names: &[String]| if names.len() <= 3 { names.join(", ") } else { format!("{}, …", names[..3].join(", ")) };
        drafts.push((wears[0].clone(), format!("I changed into {} (out of {}).", short(&wears), short(&removes))));
    }
    drafts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pocket_item_clean_and_display() {
        let it = PocketItem::clean("  iron   sword  ", "notched");
        assert_eq!(it.name, "iron sword");
        assert_eq!(it.display(), "iron sword (notched)");
        let it2 = PocketItem::parse_display("iron sword (notched)");
        assert_eq!(it2.name, "iron sword");
        assert_eq!(it2.state, "notched");
    }

    #[test]
    fn pockets_wear_and_remove_with_stash() {
        let mut p = Pockets::default();
        // wear new item
        let ops = vec![PocketOpReport { kind: PocketOpKind::Wear, item: "jacket".into(), to: "".into(), state: "".into(), where_: "".into() }];
        apply_pocket_ops(&mut p, &ops, None, 0, None);
        assert_eq!(p.worn.len(), 1);
        // remove → goes to set_aside as clothing
        let ops2 = vec![PocketOpReport { kind: PocketOpKind::Remove, item: "jacket".into(), to: "".into(), state: "".into(), where_: "".into() }];
        apply_pocket_ops(&mut p, &ops2, None, 5, None);
        assert!(p.worn.is_empty());
        assert_eq!(p.set_aside.len(), 1);
        assert!(p.set_aside[0].clothing);
        // next morning clothing expires
        p.expire_set_aside(6);
        assert!(p.set_aside.is_empty());
    }

    #[test]
    fn pockets_pickup_and_give_with_transfer() {
        let mut p = Pockets::default();
        let ops = vec![PocketOpReport { kind: PocketOpKind::Pickup, item: "car keys".into(), to: "".into(), state: "".into(), where_: "".into() }];
        apply_pocket_ops(&mut p, &ops, None, 0, None);
        assert_eq!(p.carrying.len(), 1);
        let mut transferred: Option<PocketItem> = None;
        let mut cb = |_to: String, item: PocketItem| { transferred = Some(item); };
        let give = vec![PocketOpReport { kind: PocketOpKind::Give, item: "car keys".into(), to: "Bob".into(), state: "".into(), where_: "".into() }];
        apply_pocket_ops(&mut p, &give, Some(&mut cb), 0, None);
        assert!(p.carrying.is_empty());
        assert_eq!(transferred.unwrap().name, "car keys");
    }

    #[test]
    fn pockets_setdown_and_pickup_roundtrip() {
        let mut p = Pockets::default();
        apply_pocket_ops(&mut p, &[PocketOpReport { kind: PocketOpKind::Pickup, item: "satchel".into(), to: "".into(), state: "".into(), where_: "".into() }], None, 1, None);
        apply_pocket_ops(&mut p, &[PocketOpReport { kind: PocketOpKind::Setdown, item: "satchel".into(), to: "".into(), state: "".into(), where_: "on table".into() }], None, 1, None);
        assert!(p.carrying.is_empty());
        assert_eq!(p.set_aside.len(), 1);
        assert!(!p.set_aside[0].clothing);
        // possessions never expire
        p.expire_set_aside(10);
        assert_eq!(p.set_aside.len(), 1);
        apply_pocket_ops(&mut p, &[PocketOpReport { kind: PocketOpKind::Pickup, item: "satchel".into(), to: "".into(), state: "".into(), where_: "".into() }], None, 10, None);
        assert_eq!(p.carrying.len(), 1);
        assert!(p.set_aside.is_empty());
    }

    #[test]
    fn pockets_generic_clothing_bulk() {
        let mut p = Pockets { worn: vec![PocketItem::clean("dress", ""), PocketItem::clean("shoes", "")], carrying: vec![PocketItem::clean("phone", "")], set_aside: vec![] };
        // "clothes" triggers undressAll
        apply_pocket_ops(&mut p, &[PocketOpReport { kind: PocketOpKind::Remove, item: "clothes".into(), to: "".into(), state: "".into(), where_: "".into() }], None, 1, None);
        assert!(p.worn.is_empty());
        assert!(p.carrying.is_empty());
        assert_eq!(p.set_aside.len(), 3);
    }

    #[test]
    fn pockets_same_item_containment() {
        assert!(same_item("car keys", "the car keys"));
        assert!(same_item("satchel", "worn leather satchel"));
        assert!(!same_item("car keys", "house keys"));
    }

    #[test]
    fn pockets_json_roundtrip() {
        let mut p = Pockets::default();
        apply_pocket_ops(&mut p, &[PocketOpReport { kind: PocketOpKind::Wear, item: "dress".into(), to: "".into(), state: "rain-soaked".into(), where_: "".into() }], None, 0, None);
        let j = p.to_json();
        let p2 = Pockets::from_json(&j);
        assert_eq!(p, p2);
        // set_aside filtered view
        p.set_aside.push(SetAsideItem::new(PocketItem::clean("jacket", ""), true, 1));
        assert_eq!(p.set_aside_on(1).len(), 1);
        assert_eq!(p.set_aside_on(2).len(), 0);
    }

    #[test]
    fn pockets_update_and_transform() {
        let mut p = Pockets::default();
        apply_pocket_ops(&mut p, &[PocketOpReport { kind: PocketOpKind::Pickup, item: "candy bar".into(), to: "".into(), state: "".into(), where_: "".into() }], None, 0, None);
        apply_pocket_ops(&mut p, &[PocketOpReport { kind: PocketOpKind::Update, item: "candy bar".into(), to: "".into(), state: "half-eaten".into(), where_: "".into() }], None, 0, None);
        assert_eq!(p.carrying[0].state, "half-eaten");
        apply_pocket_ops(&mut p, &[PocketOpReport { kind: PocketOpKind::Transform, item: "candy bar".into(), to: "".into(), state: "sweet wrapper".into(), where_: "".into() }], None, 0, None);
        assert_eq!(p.carrying[0].name, "sweet wrapper");
    }
}

//! What a read says, as text: sorting a raw read into what it is
//! ([`classify_name`], sc-watch's sc_detector.classify_name) and voting a
//! burst of reads into contacts ([`vote`], consensus.py).

pub mod vote;

/// What a raw read turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NameKind {
    /// A real handle, worth reporting and looking up.
    Player,
    /// An abandoned ship's serial number, not a person.
    ShipSerial,
    /// A crewed NPC vessel: "Firstname Lastname".
    NpcCrew,
    /// The game's UNKNOWN label, however it was read.
    UnknownContact,
    /// A place, creature, mineral or HUD marker ([`NON_PLAYER_WORDS`]).
    WorldLabel,
    /// Nothing usable.
    Unreadable,
}

impl NameKind {
    /// The Python's name for it (stored in the databases and logs).
    pub fn as_str(self) -> &'static str {
        match self {
            NameKind::Player => "player",
            NameKind::ShipSerial => "ship_serial",
            NameKind::NpcCrew => "npc_crew",
            NameKind::UnknownContact => "unknown_contact",
            NameKind::WorldLabel => "world_label",
            NameKind::Unreadable => "unreadable",
        }
    }
}

/// Words the HUD draws over a distance line that are not people: stars,
/// planets and moons, creatures, mineable minerals, harvestables and a few
/// HUD markers. From IRIS's exclusion list (App/Assets/exclusions.txt,
/// github.com/Niteb1rd/IRIS, GPL-3.0), its unambiguous categories only.
/// EXACT whole-word matches, deliberately: GOLD, PYRO or DIAMOND are words a
/// player could choose, so the watcher lets a match through when the handle
/// is on one of the user's own lists.
pub const NON_PLAYER_WORDS: [(&str, &str); 8] = [
    ("star", "STANTON PYRO NYX CASTRA"),
    ("planet", "HURSTON MICROTECH ARCCORP CRUSADER"),
    (
        "moon",
        "CELLIN DAYMAR YELA LYRIA WALA MAGDA ITA ARIAL ABERDEEN CLIO CALLIOPE EUTERPE MONOLITH \
         BLOOM STARLIGHT FUMAROLE",
    ),
    ("creature", "VANDUUL XIAN BANU KOPION MAROK STORMWAL"),
    (
        "mineral",
        "QUANTAINIUM BEXALITE LARANITE TARANITE AGRICIUM HEPHAESTANITE BORASE DIAMOND GOLD \
         TITANIUM COPPER CORUNDUM QUARTZ ALUMINUM BERYL AMETHYST SORENSEN DOLIVINE AMETRIN \
         JANOLITE HADALITE APHORITE",
    ),
    ("harvestable", "RANTA"),
    ("plant", "GOLDENROD PITIBU PROSTIS AMBY"),
    ("hud", "AMMUNITION DECOY NOISE"),
];

pub fn is_world_label(s: &str) -> bool {
    NON_PLAYER_WORDS
        .iter()
        .any(|(_, words)| words.split(' ').any(|w| w == s))
}

/// Python's `str.isspace()` for one character.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Punctuation trimmed off a read's ends: a recogniser's guess at glyphs it
/// lost, never part of a handle.
const END_PUNCTUATION: &str = ".,:;'\"`|/\\()[]{}*+=<>?!~^$%&";

/// `^[A-Z]{2}-\d{4}-[A-Z0-9]{2}$`: an abandoned ship's serial, SP-9695-PW.
/// (Digits are ASCII here; Python's \d also takes other scripts' digits,
/// which no reader of the HUD produces.)
fn ship_serial(s: &[u8]) -> bool {
    s.len() == 10
        && s[..2].iter().all(u8::is_ascii_uppercase)
        && s[2] == b'-'
        && s[3..7].iter().all(u8::is_ascii_digit)
        && s[7] == b'-'
        && s[8..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// `^[A-Z]{2}-\d`: a serial read part-way (readers truncate at the end of a line).
fn partial_serial(s: &[u8]) -> bool {
    s.len() >= 4
        && s[..2].iter().all(u8::is_ascii_uppercase)
        && s[2] == b'-'
        && s[3].is_ascii_digit()
}

/// A serial as the HUD reader damages it - not in the Python, which lets
/// these through as players. Measured on a Linux session's 31 serial reads
/// that passed as names: a zero read as O or Q (23 and 4 of them), the
/// prefix's O read as a zero, the first hyphen lost (11 of 29). So: the
/// prefix's 0 taken as O; in the four-digit block, the letters a digit is
/// misread as taken as digits, at least two real digits still required;
/// the first hyphen optional; the `-XX` tail and the length kept. Caught 26
/// of the 31; across 1,596 handles in the user's databases it caught no
/// real player (the 11 it did catch were cached by RSI as not existing:
/// damaged serials, looked up for nothing).
fn damaged_serial(s: &[u8]) -> bool {
    let prefix = |p: &[u8]| p.iter().all(|&c| c.is_ascii_uppercase() || c == b'0');
    // A truncated serial whose prefix O was read as a zero.
    if s.len() >= 4 && prefix(&s[..2]) && s[2] == b'-' && s[3].is_ascii_digit() {
        return true;
    }
    let n = s.len();
    if !(9..=10).contains(&n)
        || s[n - 3] != b'-'
        || !s[n - 2..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return false;
    }
    let (head, mut block) = s[..n - 3].split_at(2);
    if block.first() == Some(&b'-') {
        block = &block[1..];
    }
    let digit_like = |c: &u8| c.is_ascii_digit() || b"OQDILZSGB".contains(c);
    block.len() == 4
        && prefix(head)
        && block.iter().filter(|c| c.is_ascii_digit()).count() >= 2
        && block.iter().all(digit_like)
}

/// `^[A-Za-z0-9_-]{3,40}$`.
fn handle(s: &str) -> bool {
    (3..=40).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// The game's UNKNOWN label, however badly read - length-bounded, because
/// [`vote::same_contact`] takes a prefix as a match and 'UNKNOWNSOLDIER' is
/// an ordinary handle. The label is seven characters; damaged reads seen
/// have been nine or fewer.
pub fn is_unknown_label(s: &str) -> bool {
    s.chars().count() <= 10 && vote::same_contact(s, "UNKNOWN")
}

/// Sorts a raw read into what it actually is: (kind, the cleaned text).
///
/// An internal space means an NPC crew name - tested BEFORE anything is
/// stripped, or "JOHN SMITH" would become the handle "JOHNSMITH".
pub fn classify_name(raw: &str) -> (NameKind, Option<String>) {
    let s = raw.trim_matches(is_space);
    if s.is_empty() {
        return (NameKind::Unreadable, None);
    }
    if s.chars().any(is_space) {
        return (NameKind::NpcCrew, Some(s.to_string()));
    }
    let upper = s.to_uppercase();
    let s = upper.trim_matches(|c| END_PUNCTUATION.contains(c));
    if ship_serial(s.as_bytes()) || partial_serial(s.as_bytes()) || damaged_serial(s.as_bytes()) {
        return (NameKind::ShipSerial, Some(s.to_string()));
    }
    if !handle(s) {
        return (NameKind::Unreadable, None);
    }
    if is_unknown_label(s) {
        return (NameKind::UnknownContact, Some(s.to_string()));
    }
    if is_world_label(s) {
        return (NameKind::WorldLabel, Some(s.to_string()));
    }
    (NameKind::Player, Some(s.to_string()))
}

//! Hexagram + horoscope casting system — pure arithmetic, no external deps.
//!
//! Ports the casting logic from `myoo/mingwang/memex/casting.py`.
//! Yarrow stalk probabilities from entropy bytes, King Wen table lookup,
//! trigram-to-emotion mapping, zodiac from epoch.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// King Wen sequence: `[upper_trigram][lower_trigram]` -> hexagram number (1-64).
/// Trigram indices: 0=Ch'ien(Heaven), 1=Tui(Lake), 2=Li(Fire), 3=Chen(Thunder),
///                  4=Sun(Wind), 5=K'an(Water), 6=Ken(Mountain), 7=K'un(Earth)
const KING_WEN: [[u8; 8]; 8] = [
    [1, 10, 13, 25, 9, 6, 33, 12],    // Ch'ien (Heaven)
    [43, 58, 49, 17, 28, 47, 31, 45], // Tui (Lake)
    [14, 38, 30, 21, 50, 64, 56, 35], // Li (Fire)
    [34, 54, 55, 51, 32, 3, 62, 16],  // Chen (Thunder)
    [44, 61, 37, 42, 57, 48, 53, 20], // Sun (Wind)
    [5, 60, 63, 40, 59, 29, 4, 7],    // K'an (Water)
    [26, 41, 22, 27, 18, 39, 52, 15], // Ken (Mountain)
    [11, 19, 36, 24, 46, 8, 23, 2],   // K'un (Earth)
];

/// Hexagram names (1-indexed, index 0 is placeholder).
pub const HEXAGRAM_NAMES: [&str; 65] = [
    "",                            // 0 (unused)
    "The Creative",                // 1
    "The Receptive",               // 2
    "Difficulty at the Beginning", // 3
    "Youthful Folly",              // 4
    "Waiting",                     // 5
    "Conflict",                    // 6
    "The Army",                    // 7
    "Holding Together",            // 8
    "Small Taming",                // 9
    "Treading",                    // 10
    "Peace",                       // 11
    "Standstill",                  // 12
    "Fellowship",                  // 13
    "Great Possession",            // 14
    "Modesty",                     // 15
    "Enthusiasm",                  // 16
    "Following",                   // 17
    "Work on the Decayed",         // 18
    "Approach",                    // 19
    "Contemplation",               // 20
    "Biting Through",              // 21
    "Grace",                       // 22
    "Splitting Apart",             // 23
    "Return",                      // 24
    "Innocence",                   // 25
    "Great Taming",                // 26
    "Nourishment",                 // 27
    "Great Preponderance",         // 28
    "The Abysmal",                 // 29
    "The Clinging",                // 30
    "Influence",                   // 31
    "Duration",                    // 32
    "Retreat",                     // 33
    "Great Power",                 // 34
    "Progress",                    // 35
    "Darkening of the Light",      // 36
    "The Family",                  // 37
    "Opposition",                  // 38
    "Obstruction",                 // 39
    "Deliverance",                 // 40
    "Decrease",                    // 41
    "Increase",                    // 42
    "Breakthrough",                // 43
    "Coming to Meet",              // 44
    "Gathering Together",          // 45
    "Pushing Upward",              // 46
    "Oppression",                  // 47
    "The Well",                    // 48
    "Revolution",                  // 49
    "The Cauldron",                // 50
    "The Arousing",                // 51
    "Keeping Still",               // 52
    "Development",                 // 53
    "The Marrying Maiden",         // 54
    "Abundance",                   // 55
    "The Wanderer",                // 56
    "The Gentle",                  // 57
    "The Joyous",                  // 58
    "Dispersion",                  // 59
    "Limitation",                  // 60
    "Inner Truth",                 // 61
    "Small Preponderance",         // 62
    "After Completion",            // 63
    "Before Completion",           // 64
];

/// Trigram names.
pub const TRIGRAM_NAMES: [&str; 8] = [
    "Heaven", "Lake", "Fire", "Thunder", "Wind", "Water", "Mountain", "Earth",
];

/// Trigram -> canonical emotion (1:1 mapping).
pub const TRIGRAM_EMOTIONS: [&str; 8] = [
    "joy",      // Heaven
    "sadness",  // Lake
    "anger",    // Fire
    "surprise", // Thunder
    "interest", // Wind
    "fear",     // Water
    "boredom",  // Mountain
    "trust",    // Earth
];

/// Zodiac signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZodiacSign {
    Aries,
    Taurus,
    Gemini,
    Cancer,
    Leo,
    Virgo,
    Libra,
    Scorpio,
    Sagittarius,
    Capricorn,
    Aquarius,
    Pisces,
}

impl ZodiacSign {
    pub fn name(self) -> &'static str {
        match self {
            Self::Aries => "Aries",
            Self::Taurus => "Taurus",
            Self::Gemini => "Gemini",
            Self::Cancer => "Cancer",
            Self::Leo => "Leo",
            Self::Virgo => "Virgo",
            Self::Libra => "Libra",
            Self::Scorpio => "Scorpio",
            Self::Sagittarius => "Sagittarius",
            Self::Capricorn => "Capricorn",
            Self::Aquarius => "Aquarius",
            Self::Pisces => "Pisces",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Element {
    Fire,
    Earth,
    Air,
    Water,
}

impl Element {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fire => "Fire",
            Self::Earth => "Earth",
            Self::Air => "Air",
            Self::Water => "Water",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Modality {
    Cardinal,
    Fixed,
    Mutable,
}

impl Modality {
    pub fn name(self) -> &'static str {
        match self {
            Self::Cardinal => "Cardinal",
            Self::Fixed => "Fixed",
            Self::Mutable => "Mutable",
        }
    }
}

/// Result of casting a single hexagram line using yarrow stalk probabilities.
/// 6 = Old Yin, 7 = Young Yang, 8 = Young Yin, 9 = Old Yang
fn cast_line_yarrow(byte: u8) -> u8 {
    let r = byte as f32 / 256.0;
    if r < 0.0625 {
        6 // Old Yin    1/16
    } else if r < 0.375 {
        7 // Young Yang 5/16
    } else if r < 0.8125 {
        8 // Young Yin  7/16
    } else {
        9 // Old Yang   3/16
    }
}

/// Stable value of a line: 6,8 -> 0 (yin), 7,9 -> 1 (yang).
fn stable_value(line: u8) -> u8 {
    match line {
        6 | 8 => 0, // yin
        7 | 9 => 1, // yang
        _ => 0,
    }
}

/// Trigram index from 3 line values (bottom to top).
fn trigram_index(lines: &[u8]) -> usize {
    assert!(lines.len() >= 3);
    let mut idx = 0usize;
    for (i, &line) in lines.iter().enumerate() {
        idx |= (stable_value(line) as usize) << i;
    }
    // Trigram index mapping: 3-bit binary -> King Wen trigram order
    // Binary 111=7=Heaven(0), 110=6=Lake(1), 101=5=Fire(2), 100=4=Thunder(3)
    // 011=3=Wind(4), 010=2=Water(5), 001=1=Mountain(6), 000=0=Earth(7)
    match idx {
        7 => 0, // Heaven (all yang)
        6 => 1, // Lake
        5 => 2, // Fire
        4 => 3, // Thunder
        3 => 4, // Wind
        2 => 5, // Water
        1 => 6, // Mountain
        0 => 7, // Earth (all yin)
        _ => 7, // fallback
    }
}

/// A cast hexagram with all derived data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HexagramCast {
    pub number: u8,
    pub name: String,
    pub lines: [u8; 6],
    pub upper_trigram: usize,
    pub lower_trigram: usize,
    pub upper_name: String,
    pub lower_name: String,
}

/// Cast a hexagram from 6 entropy bytes.
/// Lines are cast bottom (0) to top (5).
pub fn cast_hexagram(entropy: &[u8]) -> HexagramCast {
    assert!(entropy.len() >= 6, "need at least 6 entropy bytes");

    let mut lines = [0u8; 6];
    for i in 0..6 {
        lines[i] = cast_line_yarrow(entropy[i]);
    }

    let lower_trigram = trigram_index(&lines[0..3]);
    let upper_trigram = trigram_index(&lines[3..6]);

    let number = KING_WEN[upper_trigram][lower_trigram];
    let name = HEXAGRAM_NAMES[number as usize].to_string();

    HexagramCast {
        number,
        name,
        lines,
        upper_trigram,
        lower_trigram,
        upper_name: TRIGRAM_NAMES[upper_trigram].to_string(),
        lower_name: TRIGRAM_NAMES[lower_trigram].to_string(),
    }
}

/// Emotion from a trigram index.
pub fn trigram_emotion(idx: usize) -> &'static str {
    TRIGRAM_EMOTIONS[idx.min(7)]
}

/// A horoscope cast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoroscopeCast {
    pub sign: ZodiacSign,
    pub element: Element,
    pub modality: Modality,
    pub sign_name: String,
}

/// Derive zodiac sign from month and day.
pub fn zodiac_from_month_day(month: u32, day: u32) -> HoroscopeCast {
    let sign = match (month, day) {
        (3, 21..=31) | (4, 1..=19) => ZodiacSign::Aries,
        (4, 20..=30) | (5, 1..=20) => ZodiacSign::Taurus,
        (5, 21..=31) | (6, 1..=20) => ZodiacSign::Gemini,
        (6, 21..=30) | (7, 1..=22) => ZodiacSign::Cancer,
        (7, 23..=31) | (8, 1..=22) => ZodiacSign::Leo,
        (8, 23..=31) | (9, 1..=22) => ZodiacSign::Virgo,
        (9, 23..=30) | (10, 1..=22) => ZodiacSign::Libra,
        (10, 23..=31) | (11, 1..=21) => ZodiacSign::Scorpio,
        (11, 22..=30) | (12, 1..=21) => ZodiacSign::Sagittarius,
        (12, 22..=31) | (1, 1..=19) => ZodiacSign::Capricorn,
        (1, 20..=31) | (2, 1..=18) => ZodiacSign::Aquarius,
        (2, 19..=29) | (3, 1..=20) => ZodiacSign::Pisces,
        _ => ZodiacSign::Capricorn, // fallback for leap year edge cases
    };

    let (element, modality) = sign_properties(sign);

    HoroscopeCast {
        sign,
        element,
        modality,
        sign_name: sign.name().to_string(),
    }
}

/// Derive zodiac from a Unix epoch timestamp.
pub fn zodiac_from_epoch(epoch: u64) -> HoroscopeCast {
    // Convert epoch to month/day (simplified — no timezone, UTC)
    let secs_per_day: u64 = 86400;
    let days_since_epoch = epoch / secs_per_day;
    // Days from 1970-01-01
    let (_, month, day) = days_to_ymd(days_since_epoch);
    zodiac_from_month_day(month, day)
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Algorithm from Howard Hinnant's date algorithms
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}

fn sign_properties(sign: ZodiacSign) -> (Element, Modality) {
    match sign {
        ZodiacSign::Aries => (Element::Fire, Modality::Cardinal),
        ZodiacSign::Taurus => (Element::Earth, Modality::Fixed),
        ZodiacSign::Gemini => (Element::Air, Modality::Mutable),
        ZodiacSign::Cancer => (Element::Water, Modality::Cardinal),
        ZodiacSign::Leo => (Element::Fire, Modality::Fixed),
        ZodiacSign::Virgo => (Element::Earth, Modality::Mutable),
        ZodiacSign::Libra => (Element::Air, Modality::Cardinal),
        ZodiacSign::Scorpio => (Element::Water, Modality::Fixed),
        ZodiacSign::Sagittarius => (Element::Fire, Modality::Mutable),
        ZodiacSign::Capricorn => (Element::Earth, Modality::Cardinal),
        ZodiacSign::Aquarius => (Element::Air, Modality::Fixed),
        ZodiacSign::Pisces => (Element::Water, Modality::Mutable),
    }
}

/// Generate a deterministic identity seed from hexagram data.
pub fn identity_seed(hex_number: u8, lines: &[u8; 6], timestamp: u64) -> u32 {
    let input = format!(
        "{}:{}:{}",
        hex_number,
        lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join(","),
        timestamp,
    );
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish() as u32
}

/// Generate an identity vector from a seed for anchor memory.
///
/// Dimension matches the embedding model (default 768 for gtr-t5-base).
/// The seed deterministically produces a unit vector via LCG PRNG.
pub fn seed_to_vector(seed: u32) -> Vec<f32> {
    seed_to_vector_dim(seed, 768)
}

/// Generate an identity vector of a specific dimension from a seed.
pub fn seed_to_vector_dim(seed: u32, dim: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dim];
    let mut s = seed;
    for x in &mut v {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223); // LCG
        *x = (s as f32) / (u32::MAX as f32) * 2.0 - 1.0;
    }
    // Normalize
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn king_wen_completeness() {
        let mut seen = HashSet::new();
        for row in &KING_WEN {
            for &val in row {
                assert!(val >= 1 && val <= 64, "invalid hexagram number: {val}");
                seen.insert(val);
            }
        }
        assert_eq!(seen.len(), 64, "not all 64 hexagrams present");
    }

    #[test]
    fn yarrow_distribution() {
        let mut counts = [0u32; 4]; // [old_yin, young_yang, young_yin, old_yang]
        let n = 10000;
        for i in 0..n {
            let byte = ((i * 256) / n) as u8;
            match cast_line_yarrow(byte) {
                6 => counts[0] += 1,
                7 => counts[1] += 1,
                8 => counts[2] += 1,
                9 => counts[3] += 1,
                _ => panic!("invalid line value"),
            }
        }
        // Expected: 6.25%, 31.25%, 43.75%, 18.75%
        let total = n as f32;
        let old_yin = counts[0] as f32 / total;
        let young_yang = counts[1] as f32 / total;
        let young_yin = counts[2] as f32 / total;
        let old_yang = counts[3] as f32 / total;

        assert!((old_yin - 0.0625).abs() < 0.02, "old_yin={old_yin}");
        assert!(
            (young_yang - 0.3125).abs() < 0.02,
            "young_yang={young_yang}"
        );
        assert!((young_yin - 0.4375).abs() < 0.02, "young_yin={young_yin}");
        assert!((old_yang - 0.1875).abs() < 0.02, "old_yang={old_yang}");
    }

    #[test]
    fn trigram_emotions_complete() {
        for i in 0..8 {
            let emotion = trigram_emotion(i);
            assert!(!emotion.is_empty(), "trigram {i} has no emotion");
        }
    }

    #[test]
    fn cast_hexagram_basic() {
        let entropy = [128u8; 6]; // all young yin (8)
        let hex = cast_hexagram(&entropy);
        assert!(hex.number >= 1 && hex.number <= 64);
        assert!(!hex.name.is_empty());
        assert_eq!(hex.lines.len(), 6);
    }

    #[test]
    fn cast_hexagram_all_yang() {
        // Bytes >= 208 (0.8125 * 256) -> Old Yang (9) -> stable yang
        let entropy = [220u8; 6];
        let hex = cast_hexagram(&entropy);
        // All yang lines: both trigrams are Heaven (index 0)
        assert_eq!(hex.upper_trigram, 0);
        assert_eq!(hex.lower_trigram, 0);
        assert_eq!(hex.number, 1); // The Creative (Heaven over Heaven)
    }

    #[test]
    fn cast_hexagram_all_yin() {
        // Bytes < 16 (0.0625 * 256) -> Old Yin (6) -> stable yin
        let entropy = [5u8; 6];
        let hex = cast_hexagram(&entropy);
        // All yin lines: both trigrams are Earth (index 7)
        assert_eq!(hex.upper_trigram, 7);
        assert_eq!(hex.lower_trigram, 7);
        assert_eq!(hex.number, 2); // The Receptive (Earth over Earth)
    }

    #[test]
    fn zodiac_known_dates() {
        // March 25 -> Aries
        let h = zodiac_from_month_day(3, 25);
        assert_eq!(h.sign, ZodiacSign::Aries);
        assert_eq!(h.element, Element::Fire);
        assert_eq!(h.modality, Modality::Cardinal);

        // August 15 -> Leo
        let h = zodiac_from_month_day(8, 15);
        assert_eq!(h.sign, ZodiacSign::Leo);

        // January 1 -> Capricorn
        let h = zodiac_from_month_day(1, 1);
        assert_eq!(h.sign, ZodiacSign::Capricorn);

        // February 20 -> Pisces
        let h = zodiac_from_month_day(2, 20);
        assert_eq!(h.sign, ZodiacSign::Pisces);
    }

    #[test]
    fn zodiac_from_epoch_works() {
        // 2026-02-28 = roughly epoch 1772150400 (Pisces)
        let h = zodiac_from_epoch(1772150400);
        assert_eq!(h.sign, ZodiacSign::Pisces);
    }

    #[test]
    fn identity_seed_deterministic() {
        let s1 = identity_seed(1, &[7, 8, 9, 6, 7, 8], 1000);
        let s2 = identity_seed(1, &[7, 8, 9, 6, 7, 8], 1000);
        assert_eq!(s1, s2);

        let s3 = identity_seed(2, &[7, 8, 9, 6, 7, 8], 1000);
        assert_ne!(s1, s3);
    }

    #[test]
    fn seed_to_vector_normalized() {
        let v = seed_to_vector(12345);
        assert_eq!(v.len(), 768);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01);
    }

    #[test]
    fn seed_to_vector_custom_dim() {
        let v = seed_to_vector_dim(12345, 384);
        assert_eq!(v.len(), 384);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01);
    }

    #[test]
    fn days_to_ymd_known() {
        // 2026-02-28 = day 20512 from epoch
        let (y, m, d) = days_to_ymd(20512);
        assert_eq!(y, 2026);
        assert_eq!(m, 2);
        assert_eq!(d, 28);
    }
}

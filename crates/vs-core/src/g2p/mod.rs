//! English grapheme-to-phoneme conversion producing the misaki phoneme alphabet used by
//! Kokoro. This is a Rust port of the dictionary path of hexgrad/misaki (Apache-2.0):
//! gold/silver lexicons, special cases for function words, -s/-ed/-ing stemming and number
//! verbalization. There is no POS tagger, so heteronyms use a small context heuristic.
//! Out-of-vocabulary words go through compound splitting and then letter-to-sound rules.

pub mod numbers;
pub mod rules;

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

const PRIMARY: char = 'ˈ';
const SECONDARY: char = 'ˌ';
const VOWELS: &str = "AIOQWYaiuæɑɒɔəɛɜɪʊʌᵻ";
const US_TAUS: &str = "AIOWYiuæɑəɛɪɹʊʌ";
/// Punctuation that Kokoro's vocabulary knows; passed through to the phoneme string.
const PUNCTS: &str = ";:,.!?—…\"“”()";

#[derive(Clone)]
enum Entry {
    Plain(String),
    /// POS-keyed alternatives; always has DEFAULT.
    Tagged(HashMap<String, Option<String>>),
}

pub struct Lexicon {
    golds: HashMap<String, Entry>,
    silvers: HashMap<String, Entry>,
    british: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pos {
    Default,
    Verb,
    Noun,
}

impl Lexicon {
    pub fn load(gold: &Path, silver: &Path, british: bool) -> Result<Self> {
        let g = parse_dict(&std::fs::read(gold).with_context(|| format!("read {}", gold.display()))?)?;
        let s = parse_dict(&std::fs::read(silver).with_context(|| format!("read {}", silver.display()))?)?;
        Ok(Self { golds: grow(g), silvers: grow(s), british })
    }

    pub fn from_json(gold: &[u8], silver: &[u8], british: bool) -> Result<Self> {
        Ok(Self { golds: grow(parse_dict(gold)?), silvers: grow(parse_dict(silver)?), british })
    }

    fn raw(&self, word: &str, pos: Pos) -> Option<String> {
        let e = self.golds.get(word).or_else(|| self.silvers.get(word))?;
        match e {
            Entry::Plain(s) => Some(s.clone()),
            Entry::Tagged(m) => {
                let key = match pos {
                    Pos::Verb => "VERB",
                    Pos::Noun => "NOUN",
                    Pos::Default => "DEFAULT",
                };
                m.get(key).cloned().flatten().or_else(|| m.get("DEFAULT").cloned().flatten())
            }
        }
    }

    fn is_known(&self, w: &str) -> bool {
        self.golds.contains_key(w) || self.silvers.contains_key(w)
    }

    fn spell_letters(&self, word: &str) -> Option<String> {
        let mut ps = String::new();
        for c in word.chars().filter(|c| c.is_alphabetic()) {
            ps.push_str(&self.raw(&c.to_uppercase().to_string(), Pos::Default)?);
        }
        let ps = apply_stress(&ps, 0.0);
        // Primary stress on the last letter: "ˌjuˌɛsˈA"
        match ps.rfind(SECONDARY) {
            Some(i) => {
                let mut s = ps.clone();
                s.replace_range(i..i + SECONDARY.len_utf8(), &PRIMARY.to_string());
                Some(s)
            }
            None => Some(ps),
        }
    }

    fn suffix_s(&self, stem: &str) -> String {
        let last = stem.chars().last().unwrap_or(' ');
        if "ptkfθ".contains(last) {
            format!("{stem}s")
        } else if "szʃʒʧʤ".contains(last) {
            format!("{stem}{}z", if self.british { 'ɪ' } else { 'ᵻ' })
        } else {
            format!("{stem}z")
        }
    }

    fn suffix_ed(&self, stem: &str) -> String {
        let cs: Vec<char> = stem.chars().collect();
        let last = *cs.last().unwrap_or(&' ');
        if "pkfθʃsʧ".contains(last) {
            format!("{stem}t")
        } else if last == 'd' {
            format!("{stem}{}d", if self.british { 'ɪ' } else { 'ᵻ' })
        } else if last != 't' {
            format!("{stem}d")
        } else if self.british || cs.len() < 2 {
            format!("{stem}ɪd")
        } else if US_TAUS.contains(cs[cs.len() - 2]) {
            let base: String = cs[..cs.len() - 1].iter().collect();
            format!("{base}ɾᵻd")
        } else {
            format!("{stem}ᵻd")
        }
    }

    fn suffix_ing(&self, stem: &str) -> String {
        let cs: Vec<char> = stem.chars().collect();
        if !self.british && cs.len() > 1 && cs[cs.len() - 1] == 't' && US_TAUS.contains(cs[cs.len() - 2]) {
            let base: String = cs[..cs.len() - 1].iter().collect();
            return format!("{base}ɾɪŋ");
        }
        format!("{stem}ɪŋ")
    }

    fn stem_lookup(&self, w: &str, pos: Pos) -> Option<String> {
        let n = w.chars().count();
        // -s
        if n >= 3 && w.ends_with('s') {
            if !w.ends_with("ss") && self.is_known(&w[..w.len() - 1]) {
                return self.raw(&w[..w.len() - 1], Pos::Noun).map(|s| self.suffix_s(&s));
            }
            if (w.ends_with("'s") || (n > 4 && w.ends_with("es") && !w.ends_with("ies"))) && self.is_known(&w[..w.len() - 2]) {
                return self.raw(&w[..w.len() - 2], Pos::Noun).map(|s| self.suffix_s(&s));
            }
            if n > 4 && w.ends_with("ies") && self.is_known(&format!("{}y", &w[..w.len() - 3])) {
                return self.raw(&format!("{}y", &w[..w.len() - 3]), Pos::Noun).map(|s| self.suffix_s(&s));
            }
        }
        // -ed
        if n >= 4 && w.ends_with('d') {
            if !w.ends_with("dd") && self.is_known(&w[..w.len() - 1]) {
                return self.raw(&w[..w.len() - 1], Pos::Verb).map(|s| self.suffix_ed(&s));
            }
            if n > 4 && w.ends_with("ed") && !w.ends_with("eed") && self.is_known(&w[..w.len() - 2]) {
                return self.raw(&w[..w.len() - 2], Pos::Verb).map(|s| self.suffix_ed(&s));
            }
        }
        // -ing
        if n >= 5 && w.ends_with("ing") {
            let base = &w[..w.len() - 3];
            if n > 5 && self.is_known(base) {
                return self.raw(base, Pos::Verb).map(|s| self.suffix_ing(&s));
            }
            let with_e = format!("{base}e");
            if self.is_known(&with_e) {
                return self.raw(&with_e, Pos::Verb).map(|s| self.suffix_ing(&s));
            }
            let b: Vec<char> = base.chars().collect();
            if n > 5 && b.len() >= 2 && b[b.len() - 1] == b[b.len() - 2] {
                let short: String = b[..b.len() - 1].iter().collect();
                if self.is_known(&short) {
                    return self.raw(&short, Pos::Verb).map(|s| self.suffix_ing(&s));
                }
            }
        }
        let _ = pos;
        None
    }

    /// Phonemes for one word token (letters/apostrophes only). Never fails: falls back to
    /// compound splitting and letter-to-sound rules.
    pub fn word(&self, word: &str, pos: Pos) -> (String, Source) {
        let w = word.replace(['‘', '’'], "'");
        if let Some(p) = self.raw(&w, pos) {
            return (p, Source::Gold);
        }
        let lower = w.to_lowercase();
        let is_upper = w.chars().all(|c| !c.is_lowercase()) && w.chars().filter(|c| c.is_alphabetic()).count() > 1;
        if let Some(p) = self.raw(&lower, pos) {
            // An all-caps word that is a known lowercase word is read as a word (e.g. "STOP").
            return (p, Source::Gold);
        }
        if let Some(p) = self.stem_lookup(&lower, pos) {
            return (p, Source::Silver);
        }
        if let Some(stripped) = lower.strip_suffix('\'') {
            if let Some(p) = self.raw(stripped, pos) {
                return (p, Source::Gold);
            }
        }
        if is_upper && w.chars().count() <= 5 {
            if let Some(p) = self.spell_letters(&w) {
                return (p, Source::Spelled);
            }
        }
        if let Some(p) = self.compound(&lower) {
            return (p, Source::Compound);
        }
        if lower.chars().all(|c| c.is_ascii_alphabetic() || c == '\'') {
            return (rules::word_to_phonemes(&lower), Source::Rules);
        }
        // Non-ASCII letters (café, naïve): strip diacritics and retry rules.
        let folded: String = lower.nfd().filter(|c| c.is_ascii_alphabetic()).collect();
        if !folded.is_empty() {
            if let Some(p) = self.raw(&folded, pos) {
                return (p, Source::Gold);
            }
            return (rules::word_to_phonemes(&folded), Source::Rules);
        }
        (String::new(), Source::Unknown)
    }

    /// Split an unknown word into two known words, preferring the longest first part.
    fn compound(&self, w: &str) -> Option<String> {
        let cs: Vec<char> = w.chars().collect();
        if cs.len() < 6 {
            return None;
        }
        for cut in (3..=cs.len() - 3).rev() {
            let a: String = cs[..cut].iter().collect();
            let b: String = cs[cut..].iter().collect();
            if let (Some(pa), Some(pb)) = (
                self.raw(&a, Pos::Default).or_else(|| self.stem_lookup(&a, Pos::Default)),
                self.raw(&b, Pos::Default).or_else(|| self.stem_lookup(&b, Pos::Default)),
            ) {
                // Second part gets secondary stress.
                return Some(format!("{pa}{}", apply_stress(&pb, -1.0)));
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Gold,
    Silver,
    Spelled,
    Compound,
    Rules,
    Number,
    Unknown,
}

fn parse_dict(bytes: &[u8]) -> Result<HashMap<String, Entry>> {
    let v: HashMap<String, Value> = serde_json::from_slice(bytes).context("lexicon JSON")?;
    let mut out = HashMap::with_capacity(v.len());
    for (k, v) in v {
        match v {
            Value::String(s) => {
                out.insert(k, Entry::Plain(s));
            }
            Value::Object(m) => {
                let mm = m.into_iter().map(|(t, p)| (t, p.as_str().map(|s| s.to_string()))).collect();
                out.insert(k, Entry::Tagged(mm));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// misaki's grow_dictionary: add Capitalized/lowercase variants.
fn grow(d: HashMap<String, Entry>) -> HashMap<String, Entry> {
    let mut e = HashMap::new();
    for (k, v) in d.iter() {
        if k.chars().count() < 2 {
            continue;
        }
        let lower = k.to_lowercase();
        let cap = capitalize(k);
        if *k == lower {
            if *k != cap {
                e.insert(cap, v.clone());
            }
        } else if *k == capitalize(&lower) {
            e.insert(lower, v.clone());
        }
    }
    let mut out = e;
    out.extend(d);
    out
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// misaki apply_stress for the cases we use: 0 / 0.5 add secondary if none, -1 demote,
/// <-1 strip, >=2 promote/add primary.
pub fn apply_stress(ps: &str, stress: f32) -> String {
    let has_p = ps.contains(PRIMARY);
    let has_s = ps.contains(SECONDARY);
    if stress < -1.0 {
        return ps.replace([PRIMARY, SECONDARY], "");
    }
    if stress == -1.0 || ((stress == 0.0 || stress == -0.5) && has_p) {
        return ps.replace(SECONDARY, "").replace(PRIMARY, &SECONDARY.to_string());
    }
    if (0.0..=1.0).contains(&stress) && !has_p && !has_s {
        if !ps.chars().any(|c| VOWELS.contains(c)) {
            return ps.to_string();
        }
        return restress(&format!("{SECONDARY}{ps}"));
    }
    if stress >= 1.0 && !has_p && has_s {
        return ps.replace(SECONDARY, &PRIMARY.to_string());
    }
    if stress > 1.0 && !has_p && !has_s {
        if !ps.chars().any(|c| VOWELS.contains(c)) {
            return ps.to_string();
        }
        return restress(&format!("{PRIMARY}{ps}"));
    }
    ps.to_string()
}

/// Move each stress mark to just before the next vowel.
fn restress(ps: &str) -> String {
    let cs: Vec<char> = ps.chars().collect();
    let mut keyed: Vec<(f64, char)> = cs.iter().enumerate().map(|(i, c)| (i as f64, *c)).collect();
    for i in 0..cs.len() {
        if cs[i] == PRIMARY || cs[i] == SECONDARY {
            if let Some(j) = (i..cs.len()).find(|&j| VOWELS.contains(cs[j])) {
                keyed[i].0 = j as f64 - 0.5;
            }
        }
    }
    keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    keyed.into_iter().map(|(_, c)| c).collect()
}

// -------------------------------------------------------------------------------------
// Text normalization + tokenization
// -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    /// Already-verbalized words (numbers etc.)
    Words(Vec<String>),
    Punct(char),
}

fn abbreviation(w: &str) -> Option<&'static str> {
    Some(match w {
        "Mr." => "mister",
        "Mrs." => "missus",
        "Ms." => "miz",
        "Dr." => "doctor",
        "St." => "saint",
        "Prof." => "professor",
        "Jr." => "junior",
        "Sr." => "senior",
        "vs." | "vs" => "versus",
        "etc." => "et cetera",
        "e.g." => "for example",
        "i.e." => "that is",
        "approx." => "approximately",
        "No." => "number",
        _ => return None,
    })
}

fn tokenize(text: &str) -> Vec<(Tok, bool /*space before*/)> {
    let text: String = text.nfkc().collect::<String>().replace(['‘', '’'], "'");
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut space = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            space = true;
            i += 1;
            continue;
        }
        // Currency
        if matches!(c, '$' | '£' | '€') && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
            let (num, j) = take_number(&chars, i + 1);
            let unit = match c {
                '$' => ("dollar", "dollars", "cent", "cents"),
                '£' => ("pound", "pounds", "penny", "pence"),
                _ => ("euro", "euros", "cent", "cents"),
            };
            let clean = num.replace(',', "");
            let (int, frac) = clean.split_once('.').unwrap_or((&clean, ""));
            let cents = if frac.is_empty() { None } else { format!("{frac:0<2}")[..2].parse().ok() };
            let mut words = numbers::currency(int.parse().unwrap_or(0), cents, unit);
            let mut j = j;
            // $5 million
            if let Some((w, k)) = peek_word(&chars, j) {
                if matches!(w.as_str(), "million" | "billion" | "thousand" | "trillion") {
                    words.pop();
                    words.push(w);
                    words.push(unit.1.into());
                    j = k;
                }
            }
            out.push((Tok::Words(words), space));
            space = false;
            i = j;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let (num, mut j) = take_number(&chars, i);
            // time 10:30
            if chars.get(j) == Some(&':') && chars.get(j + 1).is_some_and(|d| d.is_ascii_digit()) {
                let (mins, k) = take_number(&chars, j + 1);
                let h: u64 = num.parse().unwrap_or(0);
                let m: u64 = mins.parse().unwrap_or(0);
                let mut words = numbers::cardinal(h);
                if m == 0 {
                    words.push("o'clock".into());
                } else if m < 10 {
                    words.push("oh".into());
                    words.extend(numbers::cardinal(m));
                } else {
                    words.extend(numbers::cardinal(m));
                }
                out.push((Tok::Words(words), space));
                space = false;
                i = k;
                continue;
            }
            // suffixes: st nd rd th s %
            let mut suffix = String::new();
            while j < chars.len() && chars[j].is_ascii_alphabetic() && suffix.len() < 3 {
                suffix.push(chars[j]);
                j += 1;
            }
            let clean = num.replace(',', "");
            let words = if matches!(suffix.to_lowercase().as_str(), "st" | "nd" | "rd" | "th") {
                numbers::ordinal(clean.parse().unwrap_or(0))
            } else if clean.contains('.') {
                numbers::decimal(&clean)
            } else if clean.len() == 4 && !num.contains(',') && (suffix.is_empty() || suffix == "s") {
                let mut y = numbers::year(clean.parse().unwrap_or(0));
                if suffix == "s" {
                    if let Some(l) = y.last_mut() {
                        *l = plural_number_word(l);
                    }
                }
                y
            } else if clean.len() > 15 || (clean.starts_with('0') && clean.len() > 1) {
                numbers::digits(&clean)
            } else {
                let mut c = numbers::cardinal(clean.parse().unwrap_or(0));
                if suffix == "s" {
                    if let Some(l) = c.last_mut() {
                        *l = plural_number_word(l);
                    }
                }
                c
            };
            let mut words = words;
            if !suffix.is_empty() && !matches!(suffix.to_lowercase().as_str(), "st" | "nd" | "rd" | "th" | "s") {
                // e.g. "5km": keep the letters as a following word
                words.push(suffix.clone());
            }
            if chars.get(j) == Some(&'%') {
                words.push("percent".into());
                j += 1;
            }
            out.push((Tok::Words(words), space));
            space = false;
            i = j;
            continue;
        }
        if c.is_alphabetic() {
            let mut j = i;
            let mut w = String::new();
            while j < chars.len() {
                let d = chars[j];
                if d.is_alphabetic() || d.is_ascii_digit() && false {
                    w.push(d);
                    j += 1;
                } else if d == '\'' && chars.get(j + 1).is_some_and(|n| n.is_alphabetic()) && !w.is_empty() {
                    w.push(d);
                    j += 1;
                } else if d == '.' && chars.get(j + 1).is_some_and(|n| n.is_alphabetic()) && w.chars().count() == 1 {
                    // Initialisms like U.S.A. / e.g.
                    w.push(d);
                    j += 1;
                } else {
                    break;
                }
            }
            // Trailing period of an abbreviation/initialism.
            let with_dot = format!("{w}.");
            if chars.get(j) == Some(&'.') && (abbreviation(&with_dot).is_some() || w.contains('.')) {
                if let Some(exp) = abbreviation(&with_dot) {
                    out.push((Tok::Words(exp.split(' ').map(String::from).collect()), space));
                    space = false;
                    i = j + 1;
                    continue;
                }
                w = with_dot;
                j += 1;
            }
            if let Some(exp) = abbreviation(&w) {
                out.push((Tok::Words(exp.split(' ').map(String::from).collect()), space));
            } else if w.contains('.') {
                out.push((Tok::Word(w.replace('.', "")), space));
            } else {
                out.push((Tok::Word(w), space));
            }
            space = false;
            i = j;
            continue;
        }
        match c {
            '%' => out.push((Tok::Words(vec!["percent".into()]), space)),
            '&' => out.push((Tok::Words(vec!["and".into()]), space)),
            '+' => out.push((Tok::Words(vec!["plus".into()]), space)),
            '@' => out.push((Tok::Words(vec!["at".into()]), space)),
            '=' => out.push((Tok::Words(vec!["equals".into()]), space)),
            '-' | '–' => {
                // Hyphen between words: just a boundary. Spaced dash: em dash pause.
                if space && chars.get(i + 1).is_some_and(|n| n.is_whitespace()) {
                    out.push((Tok::Punct('—'), true));
                }
            }
            '[' | '{' => out.push((Tok::Punct('('), space)),
            ']' | '}' => out.push((Tok::Punct(')'), space)),
            c if PUNCTS.contains(c) => out.push((Tok::Punct(c), space)),
            _ => {} // unknown symbols (emoji, *, #, /) are not spoken
        }
        space = false;
        i += 1;
    }
    out
}

fn plural_number_word(w: &str) -> String {
    if let Some(b) = w.strip_suffix('y') {
        format!("{b}ies")
    } else {
        format!("{w}s")
    }
}

fn take_number(chars: &[char], mut i: usize) -> (String, usize) {
    let mut s = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            s.push(c);
        } else if (c == ',' || c == '.') && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
            if c == '.' && s.contains('.') {
                break;
            }
            s.push(c);
        } else {
            break;
        }
        i += 1;
    }
    (s, i)
}

fn peek_word(chars: &[char], mut i: usize) -> Option<(String, usize)> {
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let start = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    (i > start).then(|| (chars[start..i].iter().collect::<String>().to_lowercase(), i))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WordInfo {
    pub text: String,
    pub phonemes: String,
    pub source: Source,
}

pub struct G2p {
    lex: Lexicon,
}

const VERB_CUES: [&str; 14] =
    ["to", "will", "would", "can", "could", "should", "must", "might", "may", "i", "we", "you", "they", "don't"];

impl G2p {
    pub fn new(lex: Lexicon) -> Self {
        Self { lex }
    }

    /// Convert text to a Kokoro phoneme string. `emphasis` forces primary stress on every
    /// word (misaki's emphasis convention).
    pub fn phonemize(&self, text: &str, emphasis: bool) -> (String, Vec<WordInfo>) {
        let toks = tokenize(text);
        // Expand into (word, space_before) / punct items.
        enum Item {
            W(String, bool, bool /*from number*/),
            P(char, bool),
        }
        let mut items = Vec::new();
        for (t, sp) in toks {
            match t {
                Tok::Word(w) => items.push(Item::W(w, sp, false)),
                Tok::Words(ws) => {
                    for (k, w) in ws.into_iter().enumerate() {
                        for (m, part) in w.split('-').enumerate() {
                            items.push(Item::W(part.to_string(), sp || k > 0 || m > 0, true));
                        }
                    }
                }
                Tok::Punct(c) => items.push(Item::P(c, sp)),
            }
        }
        // First pass: phonemes for each word.
        let mut ph: Vec<Option<(String, Source)>> = Vec::with_capacity(items.len());
        let mut prev_word = String::new();
        for it in &items {
            match it {
                Item::W(w, _, num) => {
                    let pos = if VERB_CUES.contains(&prev_word.as_str()) { Pos::Verb } else { Pos::Default };
                    let (p, mut src) = self.lex.word(w, pos);
                    if *num {
                        src = Source::Number;
                    }
                    ph.push(Some((p, src)));
                    prev_word = w.to_lowercase();
                }
                Item::P(..) => {
                    ph.push(None);
                    prev_word.clear();
                }
            }
        }
        // Second pass: context-dependent function words and assembly.
        let mut out = String::new();
        let mut infos = Vec::new();
        let mut after_opening = false;
        for idx in 0..items.len() {
            match &items[idx] {
                Item::W(w, sp, _) => {
                    let (mut p, src) = ph[idx].clone().unwrap();
                    let next_vowel = items[idx + 1..].iter().zip(&ph[idx + 1..]).find_map(|(it, p)| match (it, p) {
                        (Item::W(..), Some((p, _))) => p.chars().find(|c| *c != PRIMARY && *c != SECONDARY).map(|c| VOWELS.contains(c)),
                        _ => None,
                    });
                    let next_is_word = matches!(items.get(idx + 1), Some(Item::W(..)));
                    match w.as_str() {
                        "the" | "The" => p = if next_vowel == Some(true) && next_is_word { "ði".into() } else { "ðə".into() },
                        "a" | "A" if w == "a" || next_is_word => p = "ɐ".into(),
                        "an" | "An" => p = "ɐn".into(),
                        "to" | "To" => {
                            p = match (next_is_word, next_vowel) {
                                (true, Some(true)) => "tʊ".into(),
                                (true, Some(false)) => "tə".into(),
                                _ => p,
                            }
                        }
                        "I" => p = "ˌI".into(),
                        _ => {}
                    }
                    if emphasis {
                        p = apply_stress(&p, 2.0);
                    }
                    let _ = sp;
                    if !out.is_empty() && !after_opening {
                        out.push(' ');
                    }
                    after_opening = false;
                    out.push_str(&p);
                    infos.push(WordInfo { text: w.clone(), phonemes: p, source: src });
                }
                Item::P(c, sp) => {
                    let c = *c;
                    let opening = matches!(c, '“' | '(') || (c == '"' && *sp);
                    // Collapse repeated punctuation like "!!!" -> "!".
                    if out.ends_with(c) && c != '.' {
                        continue;
                    }
                    if (opening || c == '—') && !out.is_empty() && !after_opening {
                        out.push(' ');
                    }
                    out.push(c);
                    after_opening = opening;
                }
            }
        }
        // Normalize "..." to the ellipsis symbol Kokoro knows.
        let out = out.replace("...", "…");
        (out.trim().to_string(), infos)
    }
}

/// Kokoro v1.0 vocabulary (from the model's tokenizer.json).
pub fn kokoro_vocab() -> &'static HashMap<char, i64> {
    use std::sync::OnceLock;
    static V: OnceLock<HashMap<char, i64>> = OnceLock::new();
    V.get_or_init(|| {
        const PAIRS: &[(char, i64)] = &[
            (';', 1), (':', 2), (',', 3), ('.', 4), ('!', 5), ('?', 6), ('—', 9), ('…', 10), ('"', 11), ('(', 12),
            (')', 13), ('“', 14), ('”', 15), (' ', 16), ('\u{0303}', 17), ('ʣ', 18), ('ʥ', 19), ('ʦ', 20), ('ʨ', 21),
            ('ᵝ', 22), ('ꭧ', 23), ('A', 24), ('I', 25), ('O', 31), ('Q', 33), ('S', 35), ('T', 36), ('W', 39),
            ('Y', 41), ('ᵊ', 42), ('a', 43), ('b', 44), ('c', 45), ('d', 46), ('e', 47), ('f', 48), ('h', 50),
            ('i', 51), ('j', 52), ('k', 53), ('l', 54), ('m', 55), ('n', 56), ('o', 57), ('p', 58), ('q', 59),
            ('r', 60), ('s', 61), ('t', 62), ('u', 63), ('v', 64), ('w', 65), ('x', 66), ('y', 67), ('z', 68),
            ('ɑ', 69), ('ɐ', 70), ('ɒ', 71), ('æ', 72), ('β', 75), ('ɔ', 76), ('ɕ', 77), ('ç', 78), ('ɖ', 80),
            ('ð', 81), ('ʤ', 82), ('ə', 83), ('ɚ', 85), ('ɛ', 86), ('ɜ', 87), ('ɟ', 90), ('ɡ', 92), ('ɥ', 99),
            ('ɨ', 101), ('ɪ', 102), ('ʝ', 103), ('ɯ', 110), ('ɰ', 111), ('ŋ', 112), ('ɳ', 113), ('ɲ', 114),
            ('ɴ', 115), ('ø', 116), ('ɸ', 118), ('θ', 119), ('œ', 120), ('ɹ', 123), ('ɾ', 125), ('ɻ', 126),
            ('ʁ', 128), ('ɽ', 129), ('ʂ', 130), ('ʃ', 131), ('ʈ', 132), ('ʧ', 133), ('ʊ', 135), ('ʋ', 136),
            ('ʌ', 138), ('ɣ', 139), ('ɤ', 140), ('χ', 142), ('ʎ', 143), ('ʒ', 147), ('ʔ', 148), ('ˈ', 156),
            ('ˌ', 157), ('ː', 158), ('ʰ', 162), ('ʲ', 164), ('↓', 169), ('→', 171), ('↗', 172), ('↘', 173),
            ('ᵻ', 177),
        ];
        PAIRS.iter().copied().collect()
    })
}

/// Map a phoneme string to Kokoro token ids (unknown symbols dropped).
pub fn kokoro_tokens(phonemes: &str) -> Vec<i64> {
    let v = kokoro_vocab();
    phonemes.chars().filter_map(|c| v.get(&c).copied()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> G2p {
        let gold = serde_json::json!({
            "hello": "həlˈO", "world": "wˈɜɹld", "the": "ði", "a": "A", "apple": "ˈæpəl", "cat": "kˈæt",
            "read": {"DEFAULT": "ɹˈid", "VBD": "ɹˈɛd"}, "live": {"DEFAULT": "lˈIv", "VERB": "lˈɪv"},
            "walk": "wˈɔk", "want": "wˈɑnt", "jump": "ʤˈʌmp", "to": "tˈu", "go": "ɡˈO", "eat": "ˈit",
            "U": "jˈu", "S": "ˈɛs", "A": "ˈA", "two": "tˈu", "thousand": "θˈWzᵊnd", "twenty": "twˈɛnti",
            "six": "sˈɪks", "dollars": "dˈɑlɚz", "and": "ænd", "fifty": "fˈɪfti", "cents": "sˈɛnts",
            "three": "θɹˈi", "sun": "sˈʌn", "flower": "flˈWəɹ", "percent": "pɚsˈɛnt", "five": "fˈIv",
            "doctor": "dˈɑktəɹ", "smith": "smˈɪθ", "we": "wi"
        });
        let lex = Lexicon::from_json(gold.to_string().as_bytes(), b"{}", false).unwrap();
        G2p::new(lex)
    }

    #[test]
    fn basic_sentence() {
        let (p, _) = tiny().phonemize("Hello, world.", false);
        assert_eq!(p, "həlˈO, wˈɜɹld.");
    }

    #[test]
    fn the_and_a_context() {
        let g = tiny();
        assert_eq!(g.phonemize("the apple", false).0, "ði ˈæpəl");
        assert_eq!(g.phonemize("the cat", false).0, "ðə kˈæt");
        assert_eq!(g.phonemize("a cat", false).0, "ɐ kˈæt");
        assert_eq!(g.phonemize("to eat", false).0, "tʊ ˈit");
        assert_eq!(g.phonemize("to go", false).0, "tə ɡˈO");
    }

    #[test]
    fn heteronym_verb_cue() {
        let g = tiny();
        assert_eq!(g.phonemize("we live", false).0, "wi lˈɪv");
        assert_eq!(g.phonemize("live", false).0, "lˈIv");
    }

    #[test]
    fn stemming() {
        let g = tiny();
        assert_eq!(g.phonemize("walked", false).0, "wˈɔkt");
        assert_eq!(g.phonemize("wanted", false).0, "wˈɑntᵻd");
        assert_eq!(g.phonemize("cats", false).0, "kˈæts");
        assert_eq!(g.phonemize("jumping", false).0, "ʤˈʌmpɪŋ");
    }

    #[test]
    fn numbers_and_money() {
        let g = tiny();
        let (_, info) = g.phonemize("$3.50 and 26%", false);
        let words: Vec<_> = info.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(words, vec!["three", "dollars", "and", "fifty", "cents", "and", "twenty", "six", "percent"]);
        let (_, info) = g.phonemize("2026", false);
        let words: Vec<_> = info.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(words, vec!["twenty", "twenty", "six"]);
    }

    #[test]
    fn acronym_spelled() {
        let (p, info) = tiny().phonemize("US", false);
        assert_eq!(info[0].source, Source::Spelled);
        assert!(p.contains('ˈ'));
    }

    #[test]
    fn compound_and_rules_fallback() {
        let g = tiny();
        let (_, info) = g.phonemize("sunflower", false);
        assert_eq!(info[0].source, Source::Compound);
        let (p, info) = g.phonemize("zorblax", false);
        assert_eq!(info[0].source, Source::Rules);
        assert!(!p.is_empty());
    }

    #[test]
    fn abbreviations() {
        let (_, info) = tiny().phonemize("Dr. Smith", false);
        assert_eq!(info[0].text, "doctor");
    }

    #[test]
    fn emphasis_adds_primary() {
        let (p, _) = tiny().phonemize("and", true);
        assert!(p.contains('ˈ'));
    }

    #[test]
    fn unknown_symbols_not_spoken() {
        let (p, _) = tiny().phonemize("hello 🎉 # / * world", false);
        assert_eq!(p, "həlˈO wˈɜɹld");
    }

    #[test]
    fn tokens_only_known() {
        let t = kokoro_tokens("həlˈO, wˈɜɹld.");
        assert_eq!(t.len(), "həlˈO, wˈɜɹld.".chars().count());
        assert!(kokoro_tokens("ʀ").is_empty());
    }

    #[test]
    fn apply_stress_cases() {
        assert_eq!(apply_stress("kˈæt", -1.0), "kˌæt");
        assert_eq!(apply_stress("ænd", 2.0), "ˈænd");
        assert_eq!(apply_stress("kˈæt", -2.0), "kæt");
    }
}

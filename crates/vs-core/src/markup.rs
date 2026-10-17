//! Deterministic parser for the Voice Studio script markup.
//!
//! | Markup    | Meaning                         |
//! |-----------|---------------------------------|
//! | `"text"`  | excited / high-energy delivery  |
//! | `[text]`  | calm / restrained delivery      |
//! | `{text}`  | mystery / suspense              |
//! | `*text*`  | emphasis                        |
//! | `--`      | short pause                     |
//! | `---`     | long pause                      |
//! | untagged  | neutral                         |
//!
//! Rules (all deterministic, documented in docs/MARKUP.md):
//! * Only the ASCII straight quote `"` is markup. Typographic quotes `“ ” ‘ ’` are literal text.
//! * `\` escapes the next character: `\" \[ \] \{ \} \* \- \\` produce the literal character.
//! * Tags nest. The innermost emotion wins; emphasis is an independent flag.
//! * `"` and `*` toggle: if one is open it closes, otherwise it opens.
//! * Closing an outer tag while inner tags are open closes the inner tags too (warning).
//! * An opener that is never closed, or a closer with no opener, is a control character:
//!   it is removed (never spoken), its words keep the surrounding style, and a warning is
//!   emitted with the byte offset.
//! * A run of exactly 2 dashes is a short pause, 3 is a long pause, 4+ is a long pause with a
//!   warning. A single `-` is a literal hyphen (part of the word, e.g. `well-known`).
//! * A blank line is a paragraph pause.
//! * Every non-control character of the source appears in exactly one text span, in order.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Emotion {
    #[default]
    Neutral,
    Excited,
    Calm,
    Mystery,
}

impl Emotion {
    pub fn label(self) -> &'static str {
        match self {
            Emotion::Neutral => "neutral",
            Emotion::Excited => "excited",
            Emotion::Calm => "calm",
            Emotion::Mystery => "mystery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PauseKind {
    Short,
    Long,
    Paragraph,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Span {
    Text {
        text: String,
        emotion: Emotion,
        emphasis: bool,
        /// Byte range in the source script.
        start: usize,
        end: usize,
    },
    Pause {
        kind: PauseKind,
        start: usize,
        end: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub offset: usize,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Parsed {
    pub spans: Vec<Span>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Excited,
    Calm,
    Mystery,
    Emphasis,
}

impl Tag {
    fn open_char(self) -> char {
        match self {
            Tag::Excited => '"',
            Tag::Calm => '[',
            Tag::Mystery => '{',
            Tag::Emphasis => '*',
        }
    }
}

/// Lexical events, positioned by byte offset.
#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Char(char),
    /// A tag character; `toggle` true for `"`/`*`, `open` for brackets.
    Open(Tag),
    Close(Tag),
    Toggle(Tag),
    Pause(PauseKind, usize /*dash count*/),
    Paragraph,
}

fn lex(src: &str) -> Vec<(usize, usize, Ev)> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    let end_of = |idx: usize| -> usize {
        if idx + 1 < chars.len() {
            chars[idx + 1].0
        } else {
            src.len()
        }
    };
    while i < chars.len() {
        let (off, c) = chars[i];
        match c {
            '\\' => {
                if i + 1 < chars.len() {
                    let (_, n) = chars[i + 1];
                    if matches!(n, '"' | '[' | ']' | '{' | '}' | '*' | '-' | '\\') {
                        out.push((off, end_of(i + 1), Ev::Char(n)));
                        i += 2;
                        continue;
                    }
                }
                // A lone backslash is literal.
                out.push((off, end_of(i), Ev::Char('\\')));
                i += 1;
            }
            '"' => {
                out.push((off, end_of(i), Ev::Toggle(Tag::Excited)));
                i += 1;
            }
            '*' => {
                out.push((off, end_of(i), Ev::Toggle(Tag::Emphasis)));
                i += 1;
            }
            '[' => {
                out.push((off, end_of(i), Ev::Open(Tag::Calm)));
                i += 1;
            }
            ']' => {
                out.push((off, end_of(i), Ev::Close(Tag::Calm)));
                i += 1;
            }
            '{' => {
                out.push((off, end_of(i), Ev::Open(Tag::Mystery)));
                i += 1;
            }
            '}' => {
                out.push((off, end_of(i), Ev::Close(Tag::Mystery)));
                i += 1;
            }
            '-' => {
                let mut j = i;
                while j < chars.len() && chars[j].1 == '-' {
                    j += 1;
                }
                let n = j - i;
                let end = if j < chars.len() { chars[j].0 } else { src.len() };
                match n {
                    1 => out.push((off, end, Ev::Char('-'))),
                    2 => out.push((off, end, Ev::Pause(PauseKind::Short, 2))),
                    _ => out.push((off, end, Ev::Pause(PauseKind::Long, n))),
                }
                i = j;
            }
            '\n' | '\r' => {
                // Detect blank line: newline, optional spaces, newline.
                let mut j = i;
                let mut newlines = 0;
                while j < chars.len() && matches!(chars[j].1, '\n' | '\r' | ' ' | '\t') {
                    if chars[j].1 == '\n' {
                        newlines += 1;
                    }
                    j += 1;
                }
                let end = if j < chars.len() { chars[j].0 } else { src.len() };
                if newlines >= 2 {
                    out.push((off, end, Ev::Paragraph));
                } else {
                    out.push((off, end, Ev::Char(' ')));
                }
                i = j;
            }
            c => {
                out.push((off, end_of(i), Ev::Char(c)));
                i += 1;
            }
        }
    }
    out
}

/// Resolve which tag events are real openers/closers. Returns, for every event index, the
/// resolved meaning: Some(true)=open, Some(false)=close, None=unmatched (dropped).
fn resolve(events: &[(usize, usize, Ev)], warnings: &mut Vec<Warning>) -> Vec<Option<bool>> {
    let mut res = vec![None; events.len()];
    // Stack of (tag, event index)
    let mut stack: Vec<(Tag, usize)> = Vec::new();
    for (idx, (off, _, ev)) in events.iter().enumerate() {
        match ev {
            Ev::Paragraph => {
                // Tags never span paragraphs: anything still open is unmatched.
                for (tag, oi) in stack.drain(..).rev() {
                    res[oi] = None;
                    warnings.push(Warning {
                        offset: events[oi].0,
                        message: format!("unclosed '{}' before paragraph break; ignored", tag.open_char()),
                    });
                }
            }
            Ev::Open(tag) => {
                stack.push((*tag, idx));
                res[idx] = Some(true);
            }
            Ev::Toggle(tag) => {
                if stack.iter().any(|(t, _)| t == tag) {
                    close_to(&mut stack, &mut res, *tag, idx, events, warnings);
                } else {
                    stack.push((*tag, idx));
                    res[idx] = Some(true);
                }
            }
            Ev::Close(tag) => {
                if stack.iter().any(|(t, _)| t == tag) {
                    close_to(&mut stack, &mut res, *tag, idx, events, warnings);
                } else {
                    res[idx] = None;
                    warnings.push(Warning {
                        offset: *off,
                        message: format!("closing '{}' without an opener; ignored", close_char(*tag)),
                    });
                }
            }
            _ => {}
        }
    }
    for (tag, oi) in stack.into_iter().rev() {
        res[oi] = None;
        warnings.push(Warning {
            offset: events[oi].0,
            message: format!("unclosed '{}'; ignored", tag.open_char()),
        });
    }
    res
}

fn close_char(tag: Tag) -> char {
    match tag {
        Tag::Excited => '"',
        Tag::Calm => ']',
        Tag::Mystery => '}',
        Tag::Emphasis => '*',
    }
}

fn close_to(
    stack: &mut Vec<(Tag, usize)>,
    res: &mut [Option<bool>],
    tag: Tag,
    idx: usize,
    events: &[(usize, usize, Ev)],
    warnings: &mut Vec<Warning>,
) {
    while let Some((t, oi)) = stack.pop() {
        if t == tag {
            res[idx] = Some(false);
            return;
        }
        // Inner tag implicitly closed here. Its opener stays valid; the implicit close is
        // recorded by marking the closer index with the inner tag via a warning only.
        warnings.push(Warning {
            offset: events[idx].0,
            message: format!(
                "'{}' opened at byte {} closed implicitly by '{}'",
                t.open_char(),
                events[oi].0,
                close_char(tag)
            ),
        });
    }
}

/// Parse a script into styled spans.
pub fn parse(src: &str) -> Parsed {
    let events = lex(src);
    let mut warnings = Vec::new();
    let resolved = resolve(&events, &mut warnings);

    let mut spans: Vec<Span> = Vec::new();
    let mut stack: Vec<Tag> = Vec::new();
    let mut buf = String::new();
    let mut buf_start = 0usize;
    let mut buf_end = 0usize;

    let style = |stack: &[Tag]| -> (Emotion, bool) {
        let mut emo = Emotion::Neutral;
        let mut emph = false;
        for t in stack {
            match t {
                Tag::Excited => emo = Emotion::Excited,
                Tag::Calm => emo = Emotion::Calm,
                Tag::Mystery => emo = Emotion::Mystery,
                Tag::Emphasis => emph = true,
            }
        }
        (emo, emph)
    };

    fn flush(
        spans: &mut Vec<Span>,
        buf: &mut String,
        start: usize,
        end: usize,
        st: (Emotion, bool),
        warnings: &mut Vec<Warning>,
        span_open_off: Option<usize>,
    ) {
        if buf.is_empty() {
            return;
        }
        let text = std::mem::take(buf);
        if text.trim().is_empty() {
            // Whitespace-only content keeps word boundaries: attach to previous text span.
            if let Some(Span::Text { text: prev, end: pe, .. }) = spans.last_mut() {
                if !prev.ends_with(' ') {
                    prev.push(' ');
                }
                *pe = end;
            }
            let _ = (span_open_off, warnings);
            return;
        }
        // Merge with previous span when the style is identical.
        if let Some(Span::Text { text: prev, emotion, emphasis, end: pe, .. }) = spans.last_mut() {
            if *emotion == st.0 && *emphasis == st.1 {
                prev.push_str(&text);
                *pe = end;
                return;
            }
        }
        spans.push(Span::Text { text, emotion: st.0, emphasis: st.1, start, end });
    }

    // Track where each tag opened so empty spans can be reported.
    let mut open_offsets: Vec<(usize, bool)> = Vec::new(); // (offset, had_text)

    for (idx, (off, end, ev)) in events.iter().enumerate() {
        match ev {
            Ev::Char(c) => {
                if buf.is_empty() {
                    buf_start = *off;
                }
                buf.push(*c);
                buf_end = *end;
                if !c.is_whitespace() {
                    for o in open_offsets.iter_mut() {
                        o.1 = true;
                    }
                }
            }
            Ev::Open(tag) | Ev::Toggle(tag) | Ev::Close(tag) => {
                let Some(is_open) = resolved[idx] else {
                    // Unmatched control char: dropped. Preserve a word boundary if the
                    // character separated two words ("a]b" -> "a b" is wrong; keep "ab").
                    continue;
                };
                flush(&mut spans, &mut buf, buf_start, buf_end, style(&stack), &mut warnings, None);
                if is_open {
                    stack.push(*tag);
                    open_offsets.push((*off, false));
                } else {
                    while let Some(t) = stack.pop() {
                        let (o, had) = open_offsets.pop().unwrap_or((*off, true));
                        if t == *tag {
                            if !had {
                                warnings.push(Warning {
                                    offset: o,
                                    message: format!("empty '{}' span", tag.open_char()),
                                });
                            }
                            break;
                        }
                    }
                }
            }
            Ev::Pause(kind, n) => {
                flush(&mut spans, &mut buf, buf_start, buf_end, style(&stack), &mut warnings, None);
                if *n > 3 {
                    warnings.push(Warning {
                        offset: *off,
                        message: format!("{n} dashes treated as a long pause"),
                    });
                }
                push_pause(&mut spans, *kind, *off, *end);
            }
            Ev::Paragraph => {
                flush(&mut spans, &mut buf, buf_start, buf_end, style(&stack), &mut warnings, None);
                stack.clear();
                open_offsets.clear();
                push_pause(&mut spans, PauseKind::Paragraph, *off, *end);
            }
        }
    }
    flush(&mut spans, &mut buf, buf_start, buf_end, style(&stack), &mut warnings, None);

    // Normalize whitespace inside spans, dropping leading/trailing pauses.
    for s in spans.iter_mut() {
        if let Span::Text { text, .. } = s {
            *text = collapse_ws(text);
        }
    }
    spans.retain(|s| !matches!(s, Span::Text { text, .. } if text.trim().is_empty()));
    while matches!(spans.first(), Some(Span::Pause { kind: PauseKind::Paragraph, .. })) {
        spans.remove(0);
    }
    while matches!(spans.last(), Some(Span::Pause { kind: PauseKind::Paragraph, .. })) {
        spans.pop();
    }
    warnings.sort_by_key(|w| w.offset);
    Parsed { spans, warnings }
}

fn push_pause(spans: &mut Vec<Span>, kind: PauseKind, start: usize, end: usize) {
    // Adjacent pauses merge into the longest one.
    if let Some(Span::Pause { kind: k, end: e, .. }) = spans.last_mut() {
        let rank = |k: PauseKind| match k {
            PauseKind::Short => 0,
            PauseKind::Paragraph => 1,
            PauseKind::Long => 2,
        };
        if rank(kind) > rank(*k) {
            *k = kind;
        }
        *e = end;
        return;
    }
    spans.push(Span::Pause { kind, start, end });
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    if s.starts_with(char::is_whitespace) && !out.is_empty() {
        out.insert(0, ' ');
    }
    out
}

/// All spoken words of the script in order, ignoring markup. Used by tests and by the
/// render pipeline to assert that no words were lost.
pub fn spoken_words(parsed: &Parsed) -> Vec<String> {
    // Spans are concatenated before splitting so that emphasis inside a word
    // (`un*believ*able`) still yields one word. Pauses and emotion changes always separate
    // words: they start a new synthesis segment, so `Hello.[Calm down.]` is two words
    // "Hello." + "Calm", exactly as it will be spoken.
    let mut all = String::new();
    let mut prev_emotion: Option<Emotion> = None;
    for s in &parsed.spans {
        match s {
            Span::Text { text, emotion, .. } => {
                if prev_emotion.is_some_and(|e| e != *emotion) {
                    all.push(' ');
                }
                all.push_str(text);
                prev_emotion = Some(*emotion);
            }
            Span::Pause { .. } => {
                all.push(' ');
                prev_emotion = None;
            }
        }
    }
    all.split_whitespace().map(|w| w.to_string()).collect()
}

/// Strip markup to the plain words it represents (for search / titles).
pub fn plain_text(src: &str) -> String {
    spoken_words(&parse(src)).join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(p: &Parsed) -> Vec<(String, Emotion, bool)> {
        p.spans
            .iter()
            .filter_map(|s| match s {
                Span::Text { text, emotion, emphasis, .. } => Some((text.trim().to_string(), *emotion, *emphasis)),
                _ => None,
            })
            .collect()
    }

    fn kinds(p: &Parsed) -> Vec<String> {
        p.spans
            .iter()
            .map(|s| match s {
                Span::Text { text, emotion, emphasis, .. } => {
                    format!("{}{}:{}", emotion.label(), if *emphasis { "+emph" } else { "" }, text.trim())
                }
                Span::Pause { kind, .. } => format!("pause:{kind:?}"),
            })
            .collect()
    }

    #[test]
    fn basic_tags() {
        let p = parse(r#"Hello "wow this rocks" then [slow down] and {who is there} *really*."#);
        assert_eq!(
            kinds(&p),
            vec![
                "neutral:Hello",
                "excited:wow this rocks",
                "neutral:then",
                "calm:slow down",
                "neutral:and",
                "mystery:who is there",
                "neutral+emph:really",
                "neutral:.",
            ]
        );
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn pauses() {
        let p = parse("One -- two --- three ---- four");
        assert_eq!(
            kinds(&p),
            vec![
                "neutral:One",
                "pause:Short",
                "neutral:two",
                "pause:Long",
                "neutral:three",
                "pause:Long",
                "neutral:four"
            ]
        );
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn single_hyphen_is_literal() {
        let p = parse("a well-known fact");
        assert_eq!(kinds(&p), vec!["neutral:a well-known fact"]);
    }

    #[test]
    fn pause_without_spaces() {
        let p = parse("wait--what");
        assert_eq!(kinds(&p), vec!["neutral:wait", "pause:Short", "neutral:what"]);
    }

    #[test]
    fn nested_innermost_emotion_wins() {
        let p = parse(r#""I can't *believe* it [quietly] okay""#);
        assert_eq!(
            texts(&p),
            vec![
                ("I can't".into(), Emotion::Excited, false),
                ("believe".into(), Emotion::Excited, true),
                ("it".into(), Emotion::Excited, false),
                ("quietly".into(), Emotion::Calm, false),
                ("okay".into(), Emotion::Excited, false),
            ]
        );
    }

    #[test]
    fn unbalanced_openers_are_dropped_words_kept() {
        let p = parse(r#"He said "hello and left"#);
        assert_eq!(spoken_words(&p), vec!["He", "said", "hello", "and", "left"]);
        assert!(texts(&p).iter().all(|t| t.1 == Emotion::Neutral));
        assert_eq!(p.warnings.len(), 1);
        assert_eq!(p.warnings[0].offset, 8);
    }

    #[test]
    fn unbalanced_closer_dropped() {
        let p = parse("calm] down } now");
        assert_eq!(spoken_words(&p), vec!["calm", "down", "now"]);
        assert_eq!(p.warnings.len(), 2);
    }

    #[test]
    fn crossing_tags_close_inner() {
        let p = parse(r#""a [b" c] d"#);
        // `"` closes `[` implicitly; trailing `]` has no opener.
        assert_eq!(spoken_words(&p), vec!["a", "b", "c", "d"]);
        let t = texts(&p);
        assert_eq!(t[0], ("a".into(), Emotion::Excited, false));
        assert_eq!(t[1], ("b".into(), Emotion::Calm, false));
        assert_eq!(t[2], ("c d".into(), Emotion::Neutral, false));
        assert!(p.warnings.len() >= 2);
    }

    #[test]
    fn escapes_produce_literals() {
        let p = parse(r#"Say \"hi\" and 5 \* 3 \[x\] \{y\} a\-\-b \\ end"#);
        assert_eq!(kinds(&p), vec![r#"neutral:Say "hi" and 5 * 3 [x] {y} a--b \ end"#]);
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn typographic_quotes_are_literal() {
        let p = parse("She said “hello” and ‘bye’");
        assert_eq!(kinds(&p), vec!["neutral:She said “hello” and ‘bye’"]);
    }

    #[test]
    fn apostrophes_untouched() {
        let p = parse(r#""Don't stop, it's John's""#);
        assert_eq!(texts(&p)[0].0, "Don't stop, it's John's");
    }

    #[test]
    fn empty_spans_warn_and_vanish() {
        let p = parse(r#"a "" b [] c {  } d ** e"#);
        assert_eq!(kinds(&p), vec!["neutral:a b c d e"]);
        assert_eq!(p.warnings.len(), 4);
    }

    #[test]
    fn unicode_preserved() {
        let p = parse("Café \"naïve 東京\" — {Ελλάδα} 🎉");
        assert_eq!(spoken_words(&p), vec!["Café", "naïve", "東京", "—", "Ελλάδα", "🎉"]);
    }

    #[test]
    fn paragraphs() {
        let p = parse("First line.\n\n\nSecond line.\nSame paragraph.");
        assert_eq!(
            kinds(&p),
            vec!["neutral:First line.", "pause:Paragraph", "neutral:Second line. Same paragraph."]
        );
    }

    #[test]
    fn tags_do_not_cross_paragraphs() {
        let p = parse("\"open here\n\nnext para\"");
        assert_eq!(spoken_words(&p), vec!["open", "here", "next", "para"]);
        assert!(texts(&p).iter().all(|t| t.1 == Emotion::Neutral));
    }

    #[test]
    fn adjacent_pauses_merge() {
        let p = parse("a -- --- b");
        assert_eq!(kinds(&p), vec!["neutral:a", "pause:Long", "neutral:b"]);
    }

    #[test]
    fn punctuation_kept_with_words() {
        let p = parse(r#"Wait... "What?!" [Fine.]"#);
        assert_eq!(spoken_words(&p), vec!["Wait...", "What?!", "Fine."]);
    }

    #[test]
    fn offsets_point_into_source() {
        let src = r#"ab "cd" ef"#;
        let p = parse(src);
        for s in &p.spans {
            if let Span::Text { text, start, end, .. } = s {
                assert!(src[*start..*end].contains(text.trim()), "{text} vs {}", &src[*start..*end]);
            }
        }
    }

    #[test]
    fn word_order_preserved_on_random_markup() {
        // Deterministic pseudo-random fuzz: inserting markup characters between words never
        // drops, duplicates or reorders words.
        let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
        let marks = ["\"", "[", "]", "{", "}", "*", " -- ", " --- ", "\n\n", ""];
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..2000 {
            let mut s = String::new();
            for w in words {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                s.push_str(marks[(seed % marks.len() as u64) as usize]);
                s.push_str(w);
                s.push_str(marks[((seed >> 8) % marks.len() as u64) as usize]);
                s.push(' ');
            }
            let p = parse(&s);
            assert_eq!(spoken_words(&p).join(" "), words.join(" "), "input: {s:?}");
        }
    }

    #[test]
    fn emotion_change_without_space_separates_words() {
        let p = parse("Hello.[Calm down.] then\"wow\"ok");
        assert_eq!(spoken_words(&p), vec!["Hello.", "Calm", "down.", "then", "wow", "ok"]);
    }

    #[test]
    fn markup_inside_word_keeps_word() {
        let p = parse("un*believ*able");
        assert_eq!(spoken_words(&p), vec!["unbelievable"]);
    }

    #[test]
    fn deterministic() {
        let s = r#"{a "b [c *d* e] f" g} -- h"#;
        let a = serde_json::to_string(&parse(s).spans).unwrap();
        let b = serde_json::to_string(&parse(s).spans).unwrap();
        assert_eq!(a, b);
    }
}

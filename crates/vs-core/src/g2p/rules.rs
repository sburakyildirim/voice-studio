//! Letter-to-sound fallback for words missing from the lexicon.
//!
//! Based on the public-domain NRL rules (Elovitz, Johnson, McHugh, Shore: "Automatic
//! Translation of English Text to Phonetics by Means of Letter-to-Sound Rules", NRL Report
//! 7948, 1976), re-targeted to the misaki/Kokoro US phoneme alphabet. This is a fallback:
//! it is intelligible but less accurate than the dictionary.
//!
//! Rule syntax: `LEFT[MATCH]RIGHT=PHONEMES`, contexts:
//! `#` one or more vowels, `:` zero or more consonants, `^` one consonant, `.` one voiced
//! consonant, `%` suffix (E, ER, ES, ED, ING, ELY), `+` front vowel (E I Y), `&` sibilant
//! (S C G Z X J CH SH), `@` (T S R D L Z N J TH CH SH), space = word boundary.

use std::sync::OnceLock;

const RULES: &str = r#"
[A] =ə
 [ARE] =ɑɹ
 [AR]O=əɹ
[AR]#=ɛɹ
 ^[AS]#=As
[A]WA=ə
[AW]=ɔ
 :[ANY]=ɛni
[A]^+#=A
#:[ALLY]=əli
 [AL]#=əl
[AGAIN]=əɡɛn
#:[AG]E=ɪʤ
[A]^+:#=æ
 :[A]^+ =A
[A]^%=A
 [ARR]=əɹ
[ARR]=æɹ
 :[AR] =ɑɹ
[AR] =ɜɹ
[AR]=ɑɹ
[AIR]=ɛɹ
[AI]=A
[AY]=A
[AU]=ɔ
#:[AL] =əl
#:[ALS] =əlz
[ALK]=ɔk
[AL]^=ɔl
 :[ABLE]=Abəl
[ABLE]=əbəl
[ANG]+=Anʤ
[ATION]=Aʃən
[A]=æ
 [BE]^#=bɪ
[BEING]=biɪŋ
 [BOTH] =bOθ
 [BUS]#=bɪz
[BUIL]=bɪl
[BB]=b
[B]=b
 [CH]^=k
^E[CH]=k
[CH]=ʧ
 S[CI]#=sI
[CI]A=ʃ
[CI]O=ʃ
[CI]EN=ʃ
[C]+=s
[CK]=k
[COM]%=kʌm
[CC]+=ks
[CC]=k
[C]=k
#:[DED] =dᵻd
.E[D] =d
#^:E[D] =t
 [DE]^#=dɪ
 [DO] =du
 [DOES]=dʌz
 [DOING]=duɪŋ
 [DOW]=dW
[DU]A=ʤu
[DD]=d
[DGE]=ʤ
[D]=d
#:[E] =
'^:[E] =
 :[E] =i
#[ED] =d
#:[E]D =
[EV]ER=ɛv
[E]^%=i
[ERI]#=iɹi
[ERI]=ɛɹɪ
#:[ER]#=ɜɹ
[ER]#=ɛɹ
[ER]=ɜɹ
 [EVEN]=ivən
#:[E]W =
@[EW]=u
[EW]=ju
[E]O=i
#:&[ES] =ᵻz
#:[E]S =
#:[ELY] =li
#:[EMENT]=mənt
[EFUL]=fʊl
[EE]=i
[EARN]=ɜɹn
 [EAR]^=ɜɹ
[EAD]=ɛd
#:[EA] =iə
[EA]SU=ɛ
[EA]=i
[EIGH]=A
[EI]=i
 [EYE]=I
[EY]=i
[EU]=ju
[E]=ɛ
[FUL]=fʊl
[FF]=f
[F]=f
[GIV]=ɡɪv
 [G]I^=ɡ
[GE]T=ɡɛ
SU[GGES]=ɡʤɛs
[GG]=ɡ
 B#[G]=ɡ
[G]+=ʤ
[GREAT]=ɡɹAt
#[GH]=
[G]=ɡ
 [HAV]=hæv
 [HERE]=hiɹ
 [HOUR]=Wəɹ
[HOW]=hW
[H]#=h
[H]=
 [IN]=ɪn
 [I] =I
[IN]D=In
[IER]=iɜɹ
#:R[IED] =id
[IED] =Id
[IEN]=iɛn
[IE]T=Iɛ
 :[I]%=I
[I]%=i
[IE]=i
[I]^+:#=ɪ
[IR]#=Iɹ
[IZ]%=Iz
[IS]%=Iz
[I]D%=I
+^[I]^+=ɪ
[I]T%=I
#^:[I]^+=ɪ
[I]^+=I
[IR]=ɜɹ
[IGH]=I
[ILD]=Ild
[IGN] =In
[IGN]^=In
[IGN]%=In
[IQUE]=ik
[I]=ɪ
[J]=ʤ
 [K]N=
[KK]=k
[K]=k
[LO]C#=lO
L[L]=
#^:[L]%=əl
[LEAD]=lid
[L]=l
[MOV]=muv
[MM]=m
[M]=m
E[NG]+=nʤ
[NG]R=ŋɡ
[NG]#=ŋɡ
[NGL]%=ŋɡəl
[NG]=ŋ
[NK]=ŋk
 [NOW] =nW
[NN]=n
[N]=n
[OF] =əv
[OROUGH]=ɜɹO
#:[OR] =ɜɹ
#:[ORS] =ɜɹz
[OR]=ɔɹ
 [ONE]=wʌn
[OW]=O
 [OVER]=Ovɜɹ
[OV]=ʌv
[O]^%=O
[O]^EN=O
[O]^I#=O
[OL]D=Ol
[OUGHT]=ɔt
[OUGH]=ʌf
 [OU]=W
H[OU]S#=W
[OUS]=əs
[OUR]=ɔɹ
[OULD]=ʊd
^[OU]^L=ʌ
[OUP]=up
[OU]=W
[OY]=Y
[OING]=Oɪŋ
[OI]=Y
[OOR]=ɔɹ
[OOK]=ʊk
[OOD]=ʊd
[OO]=u
[O]E=O
[O] =O
[OA]=O
 [ONLY]=Onli
 [ONCE]=wʌns
[ON'T]=Ont
C[O]N=ɑ
[O]NG=ɔ
 :^[O]N=ʌ
I[ON]=ən
#:[ON] =ən
#^[ON]=ən
[O]ST =O
[OF]^=ɔf
[OTHER]=ʌðɜɹ
[OSS] =ɔs
#^:[OM]=ʌm
[O]=ɑ
[PH]=f
[PEOP]=pip
[POW]=pW
[PUT] =pʊt
[PP]=p
[P]=p
[QUAR]=kwɔɹ
[QU]=kw
[Q]=k
 [RE]^#=ɹi
[RR]=ɹ
[R]=ɹ
[SH]=ʃ
#[SION]=ʒən
[SOME]=sʌm
#[SUR]#=ʒɜɹ
[SUR]#=ʃɜɹ
#[SU]#=ʒu
#[SSU]#=ʃu
#[SED] =zd
#[S]#=z
[SAID]=sɛd
^[SION]=ʃən
[S]S=
.[S] =z
#:.E[S] =z
#^:##[S] =z
#^:#[S] =s
U[S] =s
 :#[S] =z
 [SCH]=sk
[S]C+=
#[SM]=zəm
#[SN]'=zən
[S]=s
 [THE] =ðə
[TO] =tu
[THAT] =ðæt
 [THIS] =ðɪs
 [THEY]=ðA
 [THERE]=ðɛɹ
[THER]=ðɜɹ
[THEIR]=ðɛɹ
 [THAN] =ðæn
 [THEM] =ðɛm
[THESE] =ðiz
 [THEN]=ðɛn
[THROUGH]=θɹu
[THOSE]=ðOz
[THOUGH] =ðO
 [THUS]=ðʌs
[TH]=θ
#:[TED] =tᵻd
S[TI]#N=ʧ
[TI]O=ʃ
[TI]A=ʃ
[TIEN]=ʃən
[TUR]#=ʧɜɹ
[TU]A=ʧu
 [TWO]=tu
[TT]=t
[T]=t
 [UN]I=jun
 [UN]=ʌn
 [UPON]=əpɔn
@[UR]#=ʊɹ
[UR]#=jʊɹ
[UR]^=ɜɹ
[U]^ =ʌ
[U]^^=ʌ
[UY]=I
 G[U]#=
G[U]%=
G[U]#=w
#N[U]=ju
@[U]=u
[U]=ju
[VIEW]=vju
[V]=v
 [WERE]=wɜɹ
[WA]S=wɑ
[WA]T=wɑ
[WHERE]=wɛɹ
[WHAT]=wʌt
[WHOL]=hOl
[WHO]=hu
[WH]=w
[WAR]=wɔɹ
[WOR]^=wɜɹ
[WR]=ɹ
[W]=w
 [X]=z
[X]=ks
[YOUNG]=jʌŋ
 [YOU]=ju
 [YES]=jɛs
 [Y]=j
#^:[Y] =i
#^:[Y]I=i
 :[Y] =I
 :[Y]#=I
 :[Y]^+:#=ɪ
 :[Y]^#=I
[Y]=ɪ
[ZZ]=z
[Z]=z
"#;

struct Rule {
    left: Vec<char>,
    mat: Vec<char>,
    right: Vec<char>,
    out: String,
}

fn rules() -> &'static Vec<Rule> {
    static R: OnceLock<Vec<Rule>> = OnceLock::new();
    R.get_or_init(|| {
        RULES
            .lines()
            .filter(|l| l.contains('['))
            .map(|l| {
                let (lhs, out) = l.split_once('=').expect("rule has =");
                let open = lhs.find('[').unwrap();
                let close = lhs.find(']').unwrap();
                Rule {
                    left: lhs[..open].chars().collect(),
                    mat: lhs[open + 1..close].chars().collect(),
                    right: lhs[close + 1..].chars().collect(),
                    out: out.to_string(),
                }
            })
            .collect()
    })
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'A' | 'E' | 'I' | 'O' | 'U' | 'Y')
}
fn is_consonant(c: char) -> bool {
    c.is_ascii_alphabetic() && !is_vowel(c)
}
fn is_voiced(c: char) -> bool {
    matches!(c, 'B' | 'D' | 'V' | 'G' | 'J' | 'L' | 'M' | 'N' | 'R' | 'W' | 'Z')
}
fn is_front(c: char) -> bool {
    matches!(c, 'E' | 'I' | 'Y')
}

/// Match the right context starting at `pos` (index just after the match).
fn match_right(word: &[char], mut pos: usize, ctx: &[char]) -> bool {
    let mut i = 0;
    while i < ctx.len() {
        let c = ctx[i];
        let at = word.get(pos).copied().unwrap_or(' ');
        match c {
            ' ' => {
                if at != ' ' {
                    return false;
                }
                pos += 1;
            }
            '#' => {
                if !is_vowel(at) {
                    return false;
                }
                while word.get(pos).copied().is_some_and(is_vowel) {
                    pos += 1;
                }
            }
            ':' => {
                while word.get(pos).copied().is_some_and(is_consonant) {
                    pos += 1;
                }
            }
            '^' => {
                if !is_consonant(at) {
                    return false;
                }
                pos += 1;
            }
            '.' => {
                if !is_voiced(at) {
                    return false;
                }
                pos += 1;
            }
            '+' => {
                if !is_front(at) {
                    return false;
                }
                pos += 1;
            }
            '%' => {
                let rest: String = word[pos.min(word.len())..].iter().take_while(|c| **c != ' ').collect();
                let ok = ["ING", "ELY", "ER", "ES", "ED", "E"].iter().find(|s| rest.starts_with(*s));
                match ok {
                    Some(s) => pos += s.len(),
                    None => return false,
                }
            }
            lit => {
                if at != lit {
                    return false;
                }
                pos += 1;
            }
        }
        i += 1;
    }
    true
}

/// Match the left context ending just before `pos` (index of the match start).
fn match_left(word: &[char], pos: usize, ctx: &[char]) -> bool {
    let mut p = pos as isize - 1;
    let get = |p: isize| -> char { if p < 0 { ' ' } else { word.get(p as usize).copied().unwrap_or(' ') } };
    for &c in ctx.iter().rev() {
        let at = get(p);
        match c {
            ' ' => {
                if at != ' ' {
                    return false;
                }
                p -= 1;
            }
            '#' => {
                if !is_vowel(at) {
                    return false;
                }
                while p >= 0 && is_vowel(get(p)) {
                    p -= 1;
                }
            }
            ':' => {
                while p >= 0 && is_consonant(get(p)) {
                    p -= 1;
                }
            }
            '^' => {
                if !is_consonant(at) {
                    return false;
                }
                p -= 1;
            }
            '.' => {
                if !is_voiced(at) {
                    return false;
                }
                p -= 1;
            }
            '+' => {
                if !is_front(at) {
                    return false;
                }
                p -= 1;
            }
            '&' => {
                if matches!(at, 'S' | 'C' | 'G' | 'Z' | 'X' | 'J') {
                    p -= 1;
                } else if (at == 'H') && matches!(get(p - 1), 'C' | 'S') {
                    p -= 2;
                } else {
                    return false;
                }
            }
            '@' => {
                if matches!(at, 'T' | 'S' | 'R' | 'D' | 'L' | 'Z' | 'N' | 'J') {
                    p -= 1;
                } else if at == 'H' && matches!(get(p - 1), 'T' | 'C' | 'S') {
                    p -= 2;
                } else {
                    return false;
                }
            }
            lit => {
                if at != lit {
                    return false;
                }
                p -= 1;
            }
        }
    }
    true
}

const PH_VOWELS: &str = "AIOWYaiuæɑɒɔəɛɜɪʊʌᵻ";

/// Convert an alphabetic word to phonemes with a heuristic primary stress.
pub fn word_to_phonemes(word: &str) -> String {
    let up: Vec<char> = word.to_ascii_uppercase().chars().filter(|c| c.is_ascii_alphabetic() || *c == '\'').collect();
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < up.len() {
        let c = up[i];
        if c == '\'' {
            i += 1;
            continue;
        }
        for r in rules().iter().filter(|r| r.mat.first() == Some(&c)) {
            let end = i + r.mat.len();
            if end > up.len() || up[i..end] != r.mat[..] {
                continue;
            }
            if !match_left(&up, i, &r.left) || !match_right(&up, end, &r.right) {
                continue;
            }
            out.push_str(&r.out);
            i = end;
            continue 'outer;
        }
        i += 1; // no rule: skip letter
    }
    add_stress(&out, &word.to_ascii_lowercase())
}

fn add_stress(ps: &str, word: &str) -> String {
    let chars: Vec<char> = ps.chars().collect();
    let vowel_idx: Vec<usize> = chars.iter().enumerate().filter(|(_, c)| PH_VOWELS.contains(**c)).map(|(i, _)| i).collect();
    if vowel_idx.is_empty() {
        return ps.to_string();
    }
    // Collapse adjacent vowel symbols into one syllable nucleus.
    let mut nuclei = vec![vowel_idx[0]];
    for w in vowel_idx.windows(2) {
        if w[1] != w[0] + 1 {
            nuclei.push(w[1]);
        }
    }
    let unstressed_prefix = ["a", "be", "de", "re", "un", "in", "con", "com", "dis", "ex", "pre", "pro", "per", "for", "to"];
    let mut pick = 0;
    if nuclei.len() >= 2 && unstressed_prefix.iter().any(|p| word.starts_with(p) && word.len() > p.len() + 3) {
        pick = 1;
    }
    // Avoid stressing a schwa when another nucleus is available.
    if chars[nuclei[pick]] == 'ə' {
        if let Some(k) = nuclei.iter().position(|&n| chars[n] != 'ə') {
            pick = k;
        }
    }
    let at = nuclei[pick];
    let mut s: String = chars[..at].iter().collect();
    s.push('ˈ');
    s.extend(chars[at..].iter());
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rules_parse() {
        assert!(rules().len() > 250);
    }
    #[test]
    fn plausible_outputs() {
        for (w, expect) in [("cat", "kˈæt"), ("ship", "ʃˈɪp"), ("phone", "fˈOn"), ("nation", "nˈAʃən"), ("blorpington", "")] {
            let p = word_to_phonemes(w);
            assert!(!p.is_empty(), "{w}");
            if !expect.is_empty() {
                assert_eq!(p, expect, "{w}");
            }
        }
    }
}

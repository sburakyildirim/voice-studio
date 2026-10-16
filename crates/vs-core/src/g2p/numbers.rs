//! English number verbalization (cardinal, ordinal, year, decimal, currency).

const ONES: [&str; 20] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
    "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
];
const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
const SCALES: [&str; 5] = ["", "thousand", "million", "billion", "trillion"];

fn below_thousand(n: u64, out: &mut Vec<String>) {
    let h = n / 100;
    let r = n % 100;
    if h > 0 {
        out.push(ONES[h as usize].into());
        out.push("hundred".into());
    }
    if r > 0 {
        if r < 20 {
            out.push(ONES[r as usize].into());
        } else if r % 10 == 0 {
            out.push(TENS[(r / 10) as usize].into());
        } else {
            out.push(format!("{}-{}", TENS[(r / 10) as usize], ONES[(r % 10) as usize]));
        }
    }
}

pub fn cardinal(n: u64) -> Vec<String> {
    if n == 0 {
        return vec!["zero".into()];
    }
    if n >= 1_000_000_000_000_000 {
        // Too large to verbalize naturally: read digits.
        return digits(&n.to_string());
    }
    let mut groups = Vec::new();
    let mut m = n;
    while m > 0 {
        groups.push(m % 1000);
        m /= 1000;
    }
    let mut out = Vec::new();
    for (i, g) in groups.iter().enumerate().rev() {
        if *g == 0 {
            continue;
        }
        below_thousand(*g, &mut out);
        if !SCALES[i].is_empty() {
            out.push(SCALES[i].into());
        }
    }
    out
}

pub fn digits(s: &str) -> Vec<String> {
    s.chars().filter_map(|c| c.to_digit(10)).map(|d| ONES[d as usize].to_string()).collect()
}

fn ordinalize(word: &str) -> String {
    // Handle hyphenated last word ("twenty-one" -> "twenty-first").
    if let Some((a, b)) = word.rsplit_once('-') {
        return format!("{a}-{}", ordinalize(b));
    }
    match word {
        "one" => "first".into(),
        "two" => "second".into(),
        "three" => "third".into(),
        "five" => "fifth".into(),
        "eight" => "eighth".into(),
        "nine" => "ninth".into(),
        "twelve" => "twelfth".into(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    }
}

pub fn ordinal(n: u64) -> Vec<String> {
    let mut c = cardinal(n);
    if let Some(last) = c.last_mut() {
        *last = ordinalize(last);
    }
    c
}

/// Year-style reading: 1999 -> nineteen ninety-nine, 2005 -> two thousand five,
/// 2010 -> twenty ten, 1900 -> nineteen hundred.
pub fn year(n: u64) -> Vec<String> {
    if !(1000..=9999).contains(&n) {
        return cardinal(n);
    }
    let hi = n / 100;
    let lo = n % 100;
    if n % 1000 < 10 && (n / 1000) % 10 != 0 && hi % 10 == 0 {
        // 2000..2009, 3000..3009
        return cardinal(n);
    }
    let mut out = cardinal(hi);
    if lo == 0 {
        out.push("hundred".into());
    } else if lo < 10 {
        out.push("oh".into());
        out.extend(cardinal(lo));
    } else {
        out.extend(cardinal(lo));
    }
    out
}

/// Decimal like "3.14" -> three point one four.
pub fn decimal(s: &str) -> Vec<String> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut out = if int.is_empty() { vec![] } else { cardinal(int.parse().unwrap_or(0)) };
    if !frac.is_empty() {
        out.push("point".into());
        out.extend(digits(frac));
    }
    out
}

pub fn currency(int: u64, cents: Option<u64>, unit: (&str, &str, &str, &str)) -> Vec<String> {
    // unit: (singular, plural, sub singular, sub plural)
    let mut out = Vec::new();
    if int > 0 || cents.unwrap_or(0) == 0 {
        out.extend(cardinal(int));
        out.push(if int == 1 { unit.0 } else { unit.1 }.into());
    }
    if let Some(c) = cents.filter(|c| *c > 0) {
        if !out.is_empty() {
            out.push("and".into());
        }
        out.extend(cardinal(c));
        out.push(if c == 1 { unit.2 } else { unit.3 }.into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn j(v: Vec<String>) -> String {
        v.join(" ")
    }
    #[test]
    fn cardinals() {
        assert_eq!(j(cardinal(0)), "zero");
        assert_eq!(j(cardinal(21)), "twenty-one");
        assert_eq!(j(cardinal(100)), "one hundred");
        assert_eq!(j(cardinal(1_234_567)), "one million two hundred thirty-four thousand five hundred sixty-seven");
        assert_eq!(j(cardinal(1_000_001)), "one million one");
    }
    #[test]
    fn ordinals() {
        assert_eq!(j(ordinal(1)), "first");
        assert_eq!(j(ordinal(22)), "twenty-second");
        assert_eq!(j(ordinal(40)), "fortieth");
        assert_eq!(j(ordinal(112)), "one hundred twelfth");
    }
    #[test]
    fn years() {
        assert_eq!(j(year(1999)), "nineteen ninety-nine");
        assert_eq!(j(year(2005)), "two thousand five");
        assert_eq!(j(year(2010)), "twenty ten");
        assert_eq!(j(year(1900)), "nineteen hundred");
        assert_eq!(j(year(1905)), "nineteen oh five");
        assert_eq!(j(year(2026)), "twenty twenty-six");
    }
    #[test]
    fn decimals_and_money() {
        assert_eq!(j(decimal("3.14")), "three point one four");
        assert_eq!(j(currency(3, Some(50), ("dollar", "dollars", "cent", "cents"))), "three dollars and fifty cents");
        assert_eq!(j(currency(1, None, ("dollar", "dollars", "cent", "cents"))), "one dollar");
        assert_eq!(j(currency(0, Some(5), ("dollar", "dollars", "cent", "cents"))), "five cents");
    }
}

//! Fuzzy matching shared by the bundled plugins.
//!
//! Tiers, best first: prefix, substring at a word start, other substring, acronym
//! (`vsc` → Visual Studio Code), scattered subsequence, one typo (`firfox`).
//! Queries with spaces also match word by word in any order (`code visual`).
//! Matching ignores case and common Latin accents (`cafe` finds `Café`).

/// Fuzzy score in 0.0..=1.0, or None if `query` does not match `target`.
pub fn score(query: &str, target: &str) -> Option<f64> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let t = Text::new(target);
    let whole = single(&fold(query), &t);
    if !query.contains(char::is_whitespace) {
        return whole;
    }
    // Every word must match on its own, in any order.
    let mut sum = 0.0;
    let mut n = 0;
    for tok in query.split_whitespace() {
        sum += single(&fold(tok), &t)?;
        n += 1;
    }
    let by_words = sum / f64::from(n) * 0.9;
    Some(whole.map_or(by_words, |w| w.max(by_words)))
}

/// Folded (lowercase, accent-free) chars of a target plus which of them start a word.
struct Text {
    chars: Vec<char>,
    starts: Vec<bool>,
}

impl Text {
    fn new(s: &str) -> Self {
        let mut chars = Vec::with_capacity(s.len());
        let mut starts = Vec::with_capacity(s.len());
        let mut prev: Option<char> = None;
        for c in s.chars() {
            let start = match prev {
                None => true,
                Some(p) => {
                    !p.is_alphanumeric()
                        || (p.is_lowercase() && c.is_uppercase())
                        || (p.is_alphanumeric() && c.is_alphanumeric() && p.is_numeric() != c.is_numeric())
                }
            };
            let before = chars.len();
            fold_into(c, &mut chars);
            starts.extend((before..chars.len()).map(|k| start && k == before));
            prev = Some(c);
        }
        Self { chars, starts }
    }
}

fn fold(s: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        fold_into(c, &mut out);
    }
    out
}

/// Push the lowercase, accent-free form of `c` (one or two chars, e.g. ß → ss).
fn fold_into(c: char, out: &mut Vec<char>) {
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
        return;
    }
    for l in c.to_lowercase() {
        let mapped: &str = match l {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
            'ď' | 'đ' | 'ð' => "d",
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
            'ĥ' | 'ħ' => "h",
            'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
            'ĵ' => "j",
            'ķ' => "k",
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
            'ñ' | 'ń' | 'ņ' | 'ň' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
            'ŕ' | 'ŗ' | 'ř' => "r",
            'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
            'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
            'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
            'ŵ' => "w",
            'ý' | 'ÿ' | 'ŷ' => "y",
            'ź' | 'ż' | 'ž' => "z",
            'ß' => "ss",
            'æ' => "ae",
            'œ' => "oe",
            'þ' => "th",
            _ => {
                out.push(l);
                continue;
            }
        };
        out.extend(mapped.chars());
    }
}

/// Score one query word (already folded) against a target.
fn single(q: &[char], t: &Text) -> Option<f64> {
    let tc = &t.chars;
    if q.is_empty() || q.len() > tc.len() + 1 {
        return None;
    }

    if q.len() <= tc.len()
        && let Some(pos) = find_sub(tc, q)
    {
        let mut s = 0.7;
        if pos == 0 {
            s += 0.25;
        } else if t.starts[pos] {
            s += 0.15;
        }
        s += 0.05 * (q.len() as f64 / tc.len() as f64);
        return Some(s.min(1.0));
    }

    if let Some(s) = acronym(q, t) {
        return Some(s);
    }

    if let Some(s) = subsequence(q, t) {
        return Some(s);
    }

    if q.len() >= 4 && one_typo(q, t) {
        return Some(0.4);
    }
    None
}

/// Every query char on a word start, in order: `vsc` in "Visual Studio Code".
fn acronym(q: &[char], t: &Text) -> Option<f64> {
    if q.len() < 2 {
        return None;
    }
    let mut qi = 0;
    let mut words = 0;
    for (i, &c) in t.chars.iter().enumerate() {
        if !t.starts[i] || !c.is_alphanumeric() {
            continue;
        }
        words += 1;
        if qi < q.len() && c == q[qi] {
            qi += 1;
        }
    }
    (qi == q.len()).then(|| 0.75 + 0.05 * (q.len() as f64 / f64::from(words)))
}

/// Scattered match, preferring consecutive runs and word starts.
fn subsequence(q: &[char], t: &Text) -> Option<f64> {
    let mut qi = 0;
    let mut run = 0usize;
    let mut points = 0.0;
    for (ti, &c) in t.chars.iter().enumerate() {
        if qi < q.len() && c == q[qi] {
            run += 1;
            points += 1.0 + run as f64 * 0.5;
            if t.starts[ti] {
                points += 1.0;
            }
            qi += 1;
        } else {
            run = 0;
        }
    }
    if qi < q.len() {
        return None;
    }
    let max = q.len() as f64 * (1.0 + q.len() as f64 * 0.25 + 1.0);
    Some((points / max).clamp(0.0, 1.0) * 0.65)
}

/// Some word of the target, or the start of one, is at most one edit (insert, delete,
/// substitute, swap of neighbours) away from `q`.
fn one_typo(q: &[char], t: &Text) -> bool {
    let tc = &t.chars;
    let mut i = 0;
    while i < tc.len() {
        if !t.starts[i] || !tc[i].is_alphanumeric() {
            i += 1;
            continue;
        }
        let end = tc[i..].iter().position(|c| !c.is_alphanumeric()).map_or(tc.len(), |p| i + p);
        let word = &tc[i..end.min(i + q.len() + 1)];
        if word.len() + 1 >= q.len() && prefix_within_one(q, word) {
            return true;
        }
        i += 1;
    }
    false
}

/// `q` is at most one edit from `w[..L]` for some prefix length L.
fn prefix_within_one(q: &[char], w: &[char]) -> bool {
    let n = q.len();
    [n.saturating_sub(1), n, n + 1].into_iter().filter(|&l| l <= w.len() && l > 0).any(|l| within_one(q, &w[..l]))
}

/// At most one insert, delete, substitution or swap of neighbours apart. Linear, no allocation.
fn within_one(a: &[char], b: &[char]) -> bool {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    match a.len() - b.len() {
        0 => {
            let Some(i) = a.iter().zip(b).position(|(x, y)| x != y) else { return true };
            if a[i + 1..] == b[i + 1..] {
                return true; // one substitution
            }
            i + 1 < a.len() && a[i] == b[i + 1] && a[i + 1] == b[i] && a[i + 2..] == b[i + 2..]
        }
        1 => {
            let i = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(b.len());
            a[i + 1..] == b[i..]
        }
        _ => false,
    }
}

fn find_sub(hay: &[char], needle: &[char]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_beats_substring_beats_subsequence() {
        let prefix = score("fire", "Firefox").unwrap();
        let sub = score("fox", "Firefox").unwrap();
        let subseq = score("ffx", "Firefox").unwrap();
        assert!(prefix > sub && sub > subseq, "{prefix} {sub} {subseq}");
    }

    #[test]
    fn non_match_is_none() {
        assert!(score("xyz", "Firefox").is_none());
        assert!(score("", "Firefox").is_none());
        assert!(score("   ", "Firefox").is_none());
    }

    #[test]
    fn word_start_bonus() {
        assert!(score("code", "Visual Studio Code").unwrap() > score("code", "Barcode Scanner").unwrap());
    }

    #[test]
    fn case_insensitive() {
        assert!(score("FIREFOX", "firefox").is_some());
    }

    #[test]
    fn accents_fold_both_ways() {
        assert!(score("cafe", "Café").unwrap() > 0.9);
        assert!(score("café", "Cafe").unwrap() > 0.9);
        assert!(score("strasse", "Straße").unwrap() > 0.9);
        assert!(score("ubersicht", "Übersicht").unwrap() > 0.9);
    }

    #[test]
    fn acronyms_and_camel_case() {
        let vsc = score("vsc", "Visual Studio Code").unwrap();
        assert!((0.75..0.85).contains(&vsc), "{vsc}");
        assert!(score("gc", "Google Chrome").unwrap() >= 0.75);
        assert!(score("vsc", "VSCode").is_some_and(|s| s >= 0.7));
        // camelCase word start counts for the substring bonus.
        assert!(score("code", "VSCode").unwrap() > score("code", "Barcode").unwrap());
    }

    #[test]
    fn words_in_any_order() {
        let s = score("code visual", "Visual Studio Code").unwrap();
        assert!(s > 0.7, "{s}");
        assert!(score("visual code", "Visual Studio Code").is_some());
        assert!(score("code xyz", "Visual Studio Code").is_none());
    }

    #[test]
    fn one_typo_is_tolerated_for_longer_queries() {
        for q in ["firfox", "fierfox", "firefxo", "girefox", "firefoxx"] {
            assert!(score(q, "Firefox").is_some(), "{q}");
        }
        assert!(score("thunderbrid", "Mozilla Thunderbird").is_some());
        assert!(score("frfx", "Firefox").is_some(), "subsequence still works");
        assert!(score("fxie", "Firefox").is_none(), "two edits");
        assert!(score("fox", "Box").is_none(), "short queries don't fuzz");
        assert!(score("abcdef", "Firefox").is_none());
    }

    #[test]
    fn tier_order() {
        let prefix = score("visual", "Visual Studio Code").unwrap();
        let acro = score("vsc", "Visual Studio Code").unwrap();
        let subseq = score("vsud", "Visual Studio Code").unwrap();
        let typo = score("vsiual", "Visual Studio Code").unwrap();
        assert!(prefix > acro && acro > subseq && subseq > typo, "{prefix} {acro} {subseq} {typo}");
    }
}

//! Command palette logic that needs no UI: the scopes (`>` `@` `#` `!` `$` `*`),
//! splitting a typed query into scope + text, and the fuzzy matcher that
//! reports which characters matched (for the highlight).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    All,
    Commands,
    Tabs,
    Objects,
    History,
    Connections,
    /// Saved queries (`*`).
    Saved,
}

impl Scope {
    pub const ALL: [Scope; 7] = [
        Scope::All,
        Scope::Commands,
        Scope::Tabs,
        Scope::Objects,
        Scope::History,
        Scope::Connections,
        Scope::Saved,
    ];

    /// The prefix character that selects the scope.
    pub fn prefix(self) -> Option<char> {
        match self {
            Scope::All => None,
            Scope::Commands => Some('>'),
            Scope::Tabs => Some('@'),
            Scope::Objects => Some('#'),
            Scope::History => Some('!'),
            Scope::Connections => Some('$'),
            Scope::Saved => Some('*'),
        }
    }

    /// Locale key of the chip label.
    pub fn label_key(self) -> &'static str {
        match self {
            Scope::All => "pal.s_all",
            Scope::Commands => "pal.s_commands",
            Scope::Tabs => "pal.s_tabs",
            Scope::Objects => "pal.s_objects",
            Scope::History => "pal.s_history",
            Scope::Connections => "pal.s_connections",
            Scope::Saved => "sq.s_saved",
        }
    }

    /// Next scope for Tab, previous for Shift+Tab (wraps).
    pub fn cycle(self, back: bool) -> Scope {
        let i = Scope::ALL.iter().position(|s| *s == self).unwrap_or(0);
        let n = Scope::ALL.len();
        Scope::ALL[(i + if back { n - 1 } else { 1 }) % n]
    }
}

/// The scope a query asks for and the text to match. A prefix character at the
/// start wins over the chip (`chip` is what Tab last selected).
pub fn parse_query(q: &str, chip: Scope) -> (Scope, &str) {
    let t = q.trim_start();
    if let Some(c) = t.chars().next()
        && let Some(s) = Scope::ALL.iter().copied().find(|s| s.prefix() == Some(c))
    {
        return (s, t[c.len_utf8()..].trim());
    }
    (chip, t.trim_end())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub score: i32,
    /// Matched characters (indexes into `text.chars()`), for highlighting.
    pub positions: Vec<usize>,
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn boundary(prev: Option<char>) -> bool {
    prev.is_none_or(|p| !p.is_alphanumeric())
}

/// Case-insensitive fuzzy match of `query` in `text`. A contiguous hit scores
/// above a scattered one; hits at the start or after a separator score higher.
/// Empty query matches everything with score 0.
pub fn fuzzy(query: &str, text: &str) -> Option<Match> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(lower)
        .collect();
    if q.is_empty() {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    }
    let t: Vec<char> = text.chars().collect();
    let tl: Vec<char> = t.iter().copied().map(lower).collect();
    if q.len() > tl.len() {
        return None;
    }
    // Contiguous hit: the best start (word start beats a middle hit, earlier beats later).
    let mut best: Option<(i32, usize)> = None;
    for i in 0..=tl.len() - q.len() {
        if tl[i..i + q.len()] == q[..] {
            let mut s = 1000 - i as i32 * 3 - (tl.len() - q.len()) as i32;
            if boundary(i.checked_sub(1).map(|j| t[j])) {
                s += 120;
            }
            if best.is_none_or(|(b, _)| s > b) {
                best = Some((s, i));
            }
        }
    }
    if let Some((score, i)) = best {
        return Some(Match {
            score,
            positions: (i..i + q.len()).collect(),
        });
    }
    // Scattered: earliest subsequence, then prefer word starts by a second pass.
    let mut pos = Vec::with_capacity(q.len());
    let mut j = 0;
    for (i, c) in tl.iter().enumerate() {
        if j < q.len() && *c == q[j] {
            pos.push(i);
            j += 1;
        }
    }
    if j < q.len() {
        return None;
    }
    let mut score = 200;
    for (k, &p) in pos.iter().enumerate() {
        if boundary(p.checked_sub(1).map(|x| t[x])) {
            score += 15;
        }
        if k > 0 && pos[k - 1] + 1 == p {
            score += 10;
        } else if k > 0 {
            score -= (p - pos[k - 1] - 1).min(20) as i32;
        }
    }
    score -= pos[0].min(30) as i32;
    Some(Match {
        score,
        positions: pos,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn prefix_picks_the_scope_and_strips_it() {
        assert_eq!(
            parse_query("> new tab", Scope::All),
            (Scope::Commands, "new tab")
        );
        assert_eq!(parse_query("@orders", Scope::All), (Scope::Tabs, "orders"));
        assert_eq!(
            parse_query("  #users ", Scope::All),
            (Scope::Objects, "users")
        );
        assert_eq!(parse_query("!", Scope::All), (Scope::History, ""));
        assert_eq!(
            parse_query("* top users", Scope::All),
            (Scope::Saved, "top users")
        );
        assert_eq!(
            parse_query("$ prod", Scope::Tabs),
            (Scope::Connections, "prod")
        );
        // No prefix: the chip decides.
        assert_eq!(parse_query("orders ", Scope::Tabs), (Scope::Tabs, "orders"));
        assert_eq!(parse_query("", Scope::All), (Scope::All, ""));
        // A prefix character later in the text is just text.
        assert_eq!(parse_query("a>b", Scope::All), (Scope::All, "a>b"));
    }

    #[::core::prelude::v1::test]
    fn tab_cycles_through_every_scope_and_back() {
        let mut s = Scope::All;
        let mut seen = vec![s];
        for _ in 0..6 {
            s = s.cycle(false);
            seen.push(s);
        }
        assert_eq!(seen, Scope::ALL);
        assert_eq!(s.cycle(false), Scope::All);
        assert_eq!(Scope::All.cycle(true), Scope::Saved);
    }

    #[::core::prelude::v1::test]
    fn fuzzy_reports_matched_characters() {
        let m = fuzzy("ord", "orders").unwrap();
        assert_eq!(m.positions, vec![0, 1, 2]);
        let m = fuzzy("nt", "New tab").unwrap();
        assert_eq!(m.positions.len(), 2);
        let m = fuzzy("ntb", "New tab").unwrap();
        assert_eq!(m.positions, vec![0, 4, 6]);
        assert!(fuzzy("zzz", "orders").is_none());
        assert!(fuzzy("sredro", "orders").is_none());
        assert_eq!(fuzzy("", "x").unwrap().positions.len(), 0);
        // Characters, not bytes.
        let m = fuzzy("料", "資料表").unwrap();
        assert_eq!(m.positions, vec![1]);
    }

    #[::core::prelude::v1::test]
    fn better_matches_score_higher() {
        let prefix = fuzzy("ord", "orders").unwrap().score;
        let middle = fuzzy("ord", "reorder").unwrap().score;
        let word = fuzzy("ord", "sort ordering").unwrap().score;
        let scattered = fuzzy("ord", "o_r_d").unwrap().score;
        assert!(
            prefix > word && word > middle && middle > scattered,
            "{prefix} {word} {middle} {scattered}"
        );
        assert!(fuzzy("ORD", "orders").is_some());
    }
}

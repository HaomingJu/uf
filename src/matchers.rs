#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Match {
    pub score: i64,
}

pub fn fuzzy_score(query: &str, text: &str) -> Option<Match> {
    let query = normalize(query);
    if query.is_empty() {
        return Some(Match { score: 1 });
    }

    let haystack: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0i64;
    let mut last = None::<usize>;
    let mut first = None::<usize>;

    for needle in query.chars() {
        let start = last.map_or(0, |idx| idx + 1);
        let found = haystack
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, ch)| **ch == needle)
            .map(|(idx, _)| idx)?;

        if first.is_none() {
            first = Some(found);
        }

        if let Some(prev) = last {
            let gap = found.saturating_sub(prev + 1) as i64;
            score -= gap;
            if found == prev + 1 {
                score += 4;
            }
        }

        if found == 0
            || haystack
                .get(found - 1)
                .copied()
                .map(is_boundary)
                .unwrap_or(false)
        {
            score += 3;
        }

        score += 10;
        last = Some(found);
    }

    Some(Match {
        score: score - first.unwrap_or(0) as i64,
    })
}

fn normalize(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_boundary(ch: char) -> bool {
    matches!(ch, '/' | '_' | '-' | '.' | ' ' | ':' | '?')
}

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Match {
    pub score: i64,
}

pub struct FuzzyMatcher {
    matcher: Matcher,
    pattern: Pattern,
    buffer: Vec<char>,
}

impl FuzzyMatcher {
    pub fn new(query: &str) -> Self {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        Self {
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            pattern,
            buffer: Vec::new(),
        }
    }

    pub fn score(&mut self, text: &str) -> Option<Match> {
        self.pattern
            .score(Utf32Str::new(text, &mut self.buffer), &mut self.matcher)
            .map(|score| Match {
                score: i64::from(score),
            })
    }
}

pub fn fuzzy_score(query: &str, text: &str) -> Option<Match> {
    FuzzyMatcher::new(query).score(text)
}

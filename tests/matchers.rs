use web_fzf::matchers::fuzzy_score;

#[test]
fn fuzzy_score_matches_subsequence() {
    assert!(fuzzy_score("gh", "github").is_some());
}

#[test]
fn fuzzy_score_rejects_missing_char() {
    assert!(fuzzy_score("zz", "github").is_none());
}

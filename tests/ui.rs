use web_fzf::models::Entry;
use web_fzf::ui::best_entry;

#[test]
fn best_entry_prefers_tighter_match() {
    let entries = vec![
        Entry::new("GitHub Home", "https://github.com", "github", ""),
        Entry::new("GitLab Docs", "https://docs.gitlab.com", "gitlab", ""),
    ];

    let best = best_entry(&entries, "gh").unwrap();
    assert_eq!(best.url, "https://github.com");
}

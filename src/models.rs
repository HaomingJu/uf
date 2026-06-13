#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub title: String,
    pub url: String,
    pub source: String,
    pub detail: String,
}

impl Entry {
    pub fn new(
        title: impl Into<String>,
        url: impl Into<String>,
        source: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let title = title.into();
        Self {
            title: normalize_title(&title),
            url: url.into(),
            source: source.into(),
            detail: detail.into(),
        }
    }

    pub fn haystack(&self) -> String {
        format!(
            "{} {} {} {}",
            self.title, self.url, self.source, self.detail
        )
    }
}

fn normalize_title(value: &str) -> String {
    value
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '\u{200B}'
                        | '\u{200C}'
                        | '\u{200D}'
                        | '\u{200E}'
                        | '\u{200F}'
                        | '\u{061C}'
                        | '\u{2060}'
                        | '\u{FEFF}'
                        | '\u{202A}'
                        | '\u{202B}'
                        | '\u{202C}'
                        | '\u{202D}'
                        | '\u{202E}'
                        | '\u{2066}'
                        | '\u{2067}'
                        | '\u{2068}'
                        | '\u{2069}'
                )
        })
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::Entry;

    #[test]
    fn entry_new_strips_leading_whitespace_and_format_chars_from_title() {
        let entry = Entry::new(
            "\u{200f}\u{200d}   目开发代码合入记录 - 飞书云文档",
            "https://example.com",
            "bookmark",
            "",
        );

        assert_eq!(entry.title, "目开发代码合入记录 - 飞书云文档");
    }
}

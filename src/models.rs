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
        Self {
            title: title.into(),
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

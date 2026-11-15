use pinyin::ToPinyinMulti;

/// Cached character alternatives avoid transliteration during UI rendering.
#[derive(Clone)]
pub struct SearchIndex {
    original: String,
    characters: Vec<Vec<String>>,
}

impl SearchIndex {
    pub fn new(text: &str) -> Self {
        let original = text.to_lowercase();
        let characters = original.chars().map(|ch| {
            let mut alternatives = vec![ch.to_string()];
            if let Some(readings) = ch.to_pinyin_multi() {
                for reading in readings {
                    let plain = reading.plain();
                    alternatives.push(plain.to_owned());
                    alternatives.push(plain[..1].to_owned());
                }
            }
            alternatives.sort_unstable();
            alternatives.dedup();
            alternatives
        }).collect();
        Self { original, characters }
    }

    /// Contiguous matching with full syllables, initials, mixed Chinese/Latin
    /// and a partial final syllable. DP avoids enumerating heteronym combinations.
    pub fn matches(&self, query: &str) -> bool {
        if self.original.contains(query) {
            return true;
        }
        let query = query.as_bytes();
        let mut active = vec![false; query.len() + 1];
        let mut next = active.clone();
        for alternatives in &self.characters {
            active[0] = true; // A substring may begin at any character.
            next.fill(false);
            for (offset, _) in active.iter().enumerate().filter(|(_, found)| **found) {
                let remaining = &query[offset..];
                for alternative in alternatives {
                    let bytes = alternative.as_bytes();
                    if bytes.starts_with(remaining) {
                        return true;
                    }
                    if remaining.starts_with(bytes) {
                        next[offset + bytes.len()] = true;
                    }
                }
            }
            std::mem::swap(&mut active, &mut next);
        }
        false
    }
}

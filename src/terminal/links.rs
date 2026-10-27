//! Web addresses in terminal text, which open with Ctrl+click (Cmd+click on macOS).
use std::ops::Range;

/// The web addresses in `text`, as ranges of its characters.
pub fn find(text: &[char]) -> Vec<Range<usize>> {
    let mut links = vec![];
    let mut i = 0;
    while i < text.len() {
        let Some(scheme) = ["https://", "http://"]
            .into_iter()
            .find(|s| starts_with(&text[i..], s))
            .filter(|_| i == 0 || !text[i - 1].is_alphanumeric())
        else {
            i += 1;
            continue;
        };
        let start = i;
        let mut end = start + scheme.len();
        while end < text.len() && !ends_link(text[end]) {
            end += 1;
        }
        // Punctuation after an address belongs to the sentence, as do closing brackets that
        // nothing in the address opened.
        while end > start + scheme.len() {
            let last = text[end - 1];
            let unopened = |open, close| {
                last == close
                    && text[start..end].iter().filter(|c| **c == close).count()
                        > text[start..end].iter().filter(|c| **c == open).count()
            };
            if ".,;:!?'\"".contains(last) || unopened('(', ')') || unopened('[', ']') {
                end -= 1;
            } else {
                break;
            }
        }
        if end > start + scheme.len() {
            links.push(start..end);
        }
        i = end.max(start + 1);
    }
    links
}

fn starts_with(text: &[char], prefix: &str) -> bool {
    text.len() >= prefix.len()
        && prefix
            .chars()
            .zip(text)
            .all(|(p, c)| c.eq_ignore_ascii_case(&p))
}

fn ends_link(c: char) -> bool {
    c.is_whitespace() || c.is_control() || "<>\"`{}|\\^".contains(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find(&chars)
            .into_iter()
            .map(|r| chars[r].iter().collect())
            .collect()
    }

    #[test]
    fn finds_addresses_and_leaves_sentence_punctuation() {
        assert_eq!(
            links("see https://example.com/a?b=1&c=2#x, then http://10.0.0.9:8080/."),
            ["https://example.com/a?b=1&c=2#x", "http://10.0.0.9:8080/"]
        );
        assert_eq!(
            links("(docs at https://en.wikipedia.org/wiki/Rust_(language))"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(
            links("<https://example.com/path>"),
            ["https://example.com/path"]
        );
        assert_eq!(links("\"HTTPS://Example.com\""), ["HTTPS://Example.com"]);
        assert!(links("https:// alone, xhttps://example.com, ftp://host").is_empty());
    }
}

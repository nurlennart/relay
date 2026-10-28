//! Operating system commands programs send: OSC 52 to copy text to the clipboard, and OSC 8
//! to make text a link to an address it does not show.
use std::collections::VecDeque;

/// The most a program can copy at once, as base64 (about 750 KB of text).
const COPY_LIMIT: usize = 1024 * 1024;
/// Links remembered for the text on screen and in the history; older ones lose their link.
const LINKS: usize = 2000;
const URL_LIMIT: usize = 2048;

/// A link on one row: the row's text up to the link's end, which finds the row again however
/// far it has scrolled, the columns the link covers, and where it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Hyperlink {
    pub prefix: String,
    pub cols: std::ops::Range<u16>,
    pub url: String,
}

#[derive(Default)]
pub struct Osc {
    /// Text a program copied, until the window takes it.
    pub copied: Option<String>,
    /// The link being written: its address and where its text started.
    open: Option<(String, (u16, u16))>,
    pub links: VecDeque<Hyperlink>,
}

impl Osc {
    /// Takes `data`, base64 as OSC 52 sends it, as text to copy. Asking for the clipboard's
    /// contents is never answered, so a server cannot read what was copied on this computer.
    pub fn copy(&mut self, data: &[u8]) {
        if data.len() > COPY_LIMIT {
            return;
        }
        if let Some(bytes) = base64(data) {
            self.copied = Some(String::from_utf8_lossy(&bytes).into_owned());
        }
    }

    /// Handles an OSC 8 sequence: `ESC ] 8 ; params ; url` starts a link at the cursor, and
    /// the same with no address ends it.
    pub fn hyperlink(&mut self, screen: &vt100::Screen, params: &[&[u8]]) {
        // The address may itself contain semicolons, which split it into more parameters.
        let url = String::from_utf8_lossy(&params[2.min(params.len())..].join(&b';')).into_owned();
        let cursor = screen.cursor_position();
        if url.is_empty() {
            if let Some((url, start)) = self.open.take() {
                self.record(screen, &url, start, cursor);
            }
            return;
        }
        // Only web addresses open; others, such as file:// links to the server's own files,
        // mean nothing on this computer.
        let lower = url.to_ascii_lowercase();
        let web = lower.starts_with("https://") || lower.starts_with("http://");
        self.open = (web && url.len() <= URL_LIMIT).then_some((url, cursor));
    }

    /// Remembers the link written from `start` to `end`, one entry for each row it is on.
    fn record(&mut self, screen: &vt100::Screen, url: &str, start: (u16, u16), end: (u16, u16)) {
        let (_, cols) = screen.size();
        if end < start {
            return;
        }
        for row in start.0..=end.0 {
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { cols };
            if to <= from {
                continue;
            }
            self.links.push_back(Hyperlink {
                prefix: row_prefix(screen, row, to),
                cols: from..to,
                url: url.into(),
            });
            if self.links.len() > LINKS {
                self.links.pop_front();
            }
        }
    }

    /// The link at column `col` of the visible `row`, if a program made one there.
    pub fn link_at(&self, screen: &vt100::Screen, row: u16, col: u16) -> Option<&Hyperlink> {
        // Newest first: a row printed again with a different link means the latest.
        let mut prefixes = std::collections::HashMap::new();
        (self.links.iter().rev())
            .filter(|link| link.cols.contains(&col))
            .find(|link| {
                let prefix = prefixes
                    .entry(link.cols.end)
                    .or_insert_with(|| row_prefix(screen, row, link.cols.end));
                *prefix == link.prefix
            })
    }
}

/// The text of the visible `row` up to column `to`, one character a cell.
pub fn row_prefix(screen: &vt100::Screen, row: u16, to: u16) -> String {
    (0..to)
        .filter_map(|col| screen.cell(row, col))
        .filter(|c| !c.is_wide_continuation())
        .map(|c| c.contents().chars().next().unwrap_or(' '))
        .collect()
}

/// Decodes standard base64, with or without padding.
fn base64(data: &[u8]) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let data: Vec<u8> = data.iter().copied().filter(|c| *c != b'=').collect();
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= u32::from(value(*c)?) << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        match chunk.len() {
            4 => out.extend(&bytes[1..4]),
            3 => out.extend(&bytes[1..3]),
            2 => out.push(bytes[1]),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(base64(b"aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64(b"aGVsbG8gd29ybGQ").unwrap(), b"hello world");
        assert_eq!(base64(b"w6TDtsO8").unwrap(), "äöü".as_bytes());
        assert_eq!(base64(b"").unwrap(), b"");
        assert!(base64(b"a").is_none());
        assert!(base64(b"ab!d").is_none());
    }
}

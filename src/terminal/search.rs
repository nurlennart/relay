//! Finding text in the terminal's output, including the history scrolled out of view.
use std::time::{Duration, Instant};

/// A line of text as the terminal shows it, with rows that wrapped joined, and the cell each
/// character is in: a row of the whole output, counted from the oldest, and a column.
#[derive(Clone, Default)]
pub struct Line {
    pub text: Vec<char>,
    pub cells: Vec<(usize, u16)>,
}

/// Finds a query in lines handed to it one at a time, ignoring case.
pub struct Finder {
    query: Vec<char>,
    /// A line's text with case folded, kept to reuse its memory.
    folded: Vec<char>,
    /// The cells of each occurrence, oldest first.
    pub found: Vec<Vec<(usize, u16)>>,
}

impl Finder {
    pub fn new(query: &str) -> Self {
        Self {
            query: query.chars().map(fold).collect(),
            folded: vec![],
            found: vec![],
        }
    }

    pub fn line(&mut self, line: &Line) {
        let n = self.query.len();
        if n == 0 || line.text.len() < n {
            return;
        }
        self.folded.clear();
        self.folded.extend(line.text.iter().copied().map(fold));
        let mut i = 0;
        while i + n <= self.folded.len() {
            if self.folded[i..i + n] == self.query[..] {
                self.found.push(line.cells[i..i + n].to_vec());
                i += n;
            } else {
                i += 1;
            }
        }
    }
}

/// The cells of each occurrence of `query` in `lines`, oldest first, ignoring case.
#[cfg(test)]
pub fn find(lines: &[Line], query: &str) -> Vec<Vec<(usize, u16)>> {
    let mut finder = Finder::new(query);
    lines.iter().for_each(|line| finder.line(line));
    finder.found
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// While output streams in, matches are found again at most this often.
const REFRESH: Duration = Duration::from_millis(250);

/// The search bar's state.
#[derive(Default)]
pub struct Search {
    pub query: String,
    pub matches: Vec<Vec<(usize, u16)>>,
    /// The selected match, an index into `matches`.
    pub current: Option<usize>,
    /// The query and amount of output the matches are for, and when they were found.
    found: Option<(String, u64, Instant)>,
    /// Focuses the search field the next time it is shown.
    pub focus: bool,
}

impl Search {
    /// Whether the matches need finding again for `output` bytes received so far.
    pub fn stale(&self, output: u64) -> bool {
        match &self.found {
            None => true,
            Some((query, _, _)) if *query != self.query => true,
            Some((_, seen, at)) => *seen != output && at.elapsed() >= REFRESH,
        }
    }

    /// How long until output beyond the `output` bytes searched may be searched, while the
    /// matches are for less than that. The last output before a pause needs this wake-up, as
    /// none follows it.
    pub fn refresh_in(&self, output: u64) -> Option<Duration> {
        match &self.found {
            Some((_, seen, at)) if *seen != output => Some(REFRESH.saturating_sub(at.elapsed())),
            _ => None,
        }
    }

    /// Takes `matches` found for `output` bytes. A new query, or the first matches of one,
    /// selects the last match at or above `bottom`, the lowest row in view, so the search
    /// starts where the reader looks. Returns whether the selection moved to another match.
    pub fn update(&mut self, matches: Vec<Vec<(usize, u16)>>, output: u64, bottom: usize) -> bool {
        let new_query =
            self.found.as_ref().is_none_or(|(q, _, _)| *q != self.query) || self.current.is_none();
        self.found = Some((self.query.clone(), output, Instant::now()));
        let before = self.current.and_then(|i| self.matches.get(i)).map(|m| m[0]);
        self.matches = matches;
        if new_query {
            let above = self.matches.partition_point(|m| m[0].0 <= bottom);
            self.current = above
                .checked_sub(1)
                .or((!self.matches.is_empty()).then_some(0));
            return self.current.is_some();
        }
        // New output keeps the same match selected where it is still there.
        self.current = before
            .and_then(|cell| self.matches.iter().position(|m| m[0] == cell))
            .or_else(|| {
                self.current
                    .map(|i| i.min(self.matches.len().saturating_sub(1)))
            })
            .filter(|_| !self.matches.is_empty());
        false
    }

    /// Selects the match before (`older`) or after the current one, wrapping around.
    pub fn step(&mut self, older: bool) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        self.current = Some(match self.current {
            Some(i) if older => (i + count - 1) % count,
            Some(i) => (i + 1) % count,
            None => count - 1,
        });
    }

    /// "3 of 12", or "No results".
    pub fn summary(&self) -> String {
        match self.current {
            _ if self.query.is_empty() => String::new(),
            Some(i) if !self.matches.is_empty() => format!("{} of {}", i + 1, self.matches.len()),
            _ => "No results".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(row: usize, text: &str) -> Line {
        Line {
            text: text.chars().collect(),
            cells: (0..text.chars().count()).map(|c| (row, c as u16)).collect(),
        }
    }

    #[test]
    fn finds_every_occurrence_ignoring_case() {
        let lines = [
            line(0, "Error: disk full"),
            line(1, "ok"),
            line(2, "ERROR again, error"),
        ];
        let found = find(&lines, "error");
        let starts: Vec<_> = found.iter().map(|m| m[0]).collect();
        assert_eq!(starts, [(0, 0), (2, 0), (2, 13)]);
        assert_eq!(found[0].len(), 5);
        assert!(find(&lines, "").is_empty());
        // A line that wrapped is one line, so matches cross the break.
        let mut wrapped = line(5, "connection re");
        let rest = line(6, "fused");
        wrapped.text.extend(rest.text);
        wrapped.cells.extend(rest.cells);
        assert_eq!(
            find(&[wrapped], "refused")[0],
            [(5, 11), (5, 12), (6, 0), (6, 1), (6, 2), (6, 3), (6, 4)]
        );
    }

    #[test]
    fn selection_starts_in_view_and_wraps_around() {
        let lines = [line(0, "a"), line(10, "a"), line(20, "a"), line(30, "a")];
        let mut search = Search {
            query: "a".into(),
            ..Search::default()
        };
        assert!(search.stale(0));
        // The view's lowest row is 25: the match on row 20 is selected first.
        assert!(search.update(find(&lines, "a"), 0, 25));
        assert_eq!(
            (search.current, search.summary().as_str()),
            (Some(2), "3 of 4")
        );
        assert!(!search.stale(0));
        // Output right after a search waits, with a wake-up for when it may be searched.
        assert!(!search.stale(5));
        assert!(search.refresh_in(5).is_some_and(|d| d <= REFRESH));
        assert_eq!(search.refresh_in(0), None);
        search.step(true);
        search.step(true);
        search.step(true);
        assert_eq!(
            search.current,
            Some(3),
            "older from the first wraps to the newest"
        );
        search.step(false);
        assert_eq!(search.current, Some(0));
        // More output keeps the selected match.
        let more = [
            line(0, "b"),
            line(10, "a"),
            line(20, "a"),
            line(30, "a"),
            line(40, "a"),
        ];
        search.current = Some(1);
        assert!(!search.update(find(&more, "a"), 9, 45));
        assert_eq!(
            search.current,
            Some(0),
            "the match on row 10 stays selected"
        );
        // Matches that appear after the query was typed are selected.
        search.query = "b".into();
        search.update(find(&more, "zzz"), 9, 45);
        assert_eq!(search.current, None);
        search.update(find(&more, "b"), 10, 45);
        assert_eq!(search.summary(), "1 of 1");
        search.query = "zzz".into();
        assert!(search.stale(9));
        search.update(find(&more, "zzz"), 9, 45);
        assert_eq!(search.summary(), "No results");
    }
}

//! Borrow exact JSON values and cache each traversed container once.
use intentumdiff_plugin_sdk::tree::Position;
use serde_json::value::RawValue;
use std::collections::{BTreeMap, HashMap};

enum Container<'a> {
    Map(BTreeMap<String, &'a RawValue>),
    Seq(Vec<&'a RawValue>),
}

pub(crate) struct JsonSpans<'a> {
    source: &'a str,
    root: &'a RawValue,
    containers: HashMap<usize, Container<'a>>,
    lines: Vec<usize>,
}
impl<'a> JsonSpans<'a> {
    pub(crate) fn new(source: &'a str) -> Result<Self, String> {
        let root = serde_json::from_str(source).map_err(|e| e.to_string())?;
        let lines = std::iter::once(0)
            .chain(
                source
                    .bytes()
                    .enumerate()
                    .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
            )
            .collect();
        Ok(Self {
            source,
            root,
            containers: HashMap::new(),
            lines,
        })
    }
    pub(crate) fn at(&mut self, path: &[&str]) -> Option<&'a RawValue> {
        let mut raw = self.root;
        for part in path {
            let offset = raw.get().as_ptr() as usize - self.source.as_ptr() as usize;
            if let std::collections::hash_map::Entry::Vacant(entry) = self.containers.entry(offset)
            {
                let container = match raw.get().as_bytes().first()? {
                    b'{' => Container::Map(serde_json::from_str(raw.get()).ok()?),
                    b'[' => Container::Seq(serde_json::from_str(raw.get()).ok()?),
                    _ => return None,
                };
                entry.insert(container);
            }
            raw = match self.containers.get(&offset)? {
                Container::Map(map) => *map.get(*part)?,
                Container::Seq(seq) => *seq.get(part.parse::<usize>().ok()?)?,
            };
        }
        Some(raw)
    }
    fn point(&self, offset: usize) -> (u32, u32) {
        let line = self.lines.partition_point(|&start| start <= offset) - 1;
        (line as u32, (offset - self.lines[line]) as u32)
    }
    pub(crate) fn position(&mut self, path: &[&str]) -> Option<Position> {
        let raw = self.at(path)?.get();
        let start = raw.as_ptr() as usize - self.source.as_ptr() as usize;
        let (start_line, start_col) = self.point(start);
        let (end_line, end_col) = self.point(start + raw.len());
        Some(Position {
            start_line,
            start_col,
            end_line,
            end_col,
        })
    }

    /// The exact quoted key immediately before this borrowed JSON value.
    pub(crate) fn key_position(&mut self, path: &[&str]) -> Option<Position> {
        let (key, parent_path) = path.split_last()?;
        let raw = self.at(path)?.get();
        let value_start = raw.as_ptr() as usize - self.source.as_ptr() as usize;
        let parent = self.at(parent_path)?.get();
        if !parent.starts_with('{') {
            return None;
        }
        let lower = parent.as_ptr() as usize - self.source.as_ptr() as usize;
        let bytes = self.source.as_bytes();
        let mut end = value_start;
        while end > lower && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end <= lower || bytes[end - 1] != b':' {
            return None;
        }
        end -= 1;
        while end > lower && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end <= lower || bytes[end - 1] != b'"' {
            return None;
        }
        let mut start = end - 1;
        loop {
            start = start.checked_sub(1)?;
            if start <= lower {
                return None;
            }
            if bytes[start] == b'"' {
                let mut escaped = start;
                while escaped > lower && bytes[escaped - 1] == b'\\' {
                    escaped -= 1;
                }
                if (start - escaped) % 2 == 0 {
                    break;
                }
            }
        }
        let decoded: String = serde_json::from_str(self.source.get(start..end)?).ok()?;
        if decoded != *key {
            return None;
        }
        let (start_line, start_col) = self.point(start);
        let (end_line, end_col) = self.point(end);
        Some(Position {
            start_line,
            start_col,
            end_line,
            end_col,
        })
    }
}

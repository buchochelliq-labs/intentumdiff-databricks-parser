use intentumdiff_plugin_sdk::tree::{Position, SemanticNode};
use std::collections::{BTreeMap, HashMap};
use yaml_rust2::{
    parser::{Event, MarkedEventReceiver, Parser},
    scanner::Marker,
};

#[derive(Clone)]
enum Children {
    Map(BTreeMap<String, Span>),
    Seq(Vec<Span>),
    Scalar(String),
}
#[derive(Clone)]
struct Span {
    position: Position,
    children: Children,
}
struct Events(Vec<(Event, Marker)>);
impl MarkedEventReceiver for Events {
    fn on_event(&mut self, event: Event, mark: Marker) {
        self.0.push((event, mark));
    }
}

// Index lines once; only non-ASCII lines need character-to-byte tables.
struct SourceMap<'a> {
    lines: Vec<&'a str>,
    unicode: HashMap<usize, Vec<usize>>,
}
impl<'a> SourceMap<'a> {
    fn new(source: &'a str) -> Self {
        let lines: Vec<_> = source.split('\n').collect();
        let unicode = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !line.is_ascii())
            .map(|(i, line)| {
                (
                    i,
                    line.char_indices()
                        .map(|(byte, _)| byte)
                        .chain(std::iter::once(line.len()))
                        .collect(),
                )
            })
            .collect();
        Self { lines, unicode }
    }
    fn point(&self, mark: Marker) -> (u32, u32) {
        let line = mark.line().saturating_sub(1);
        let Some(text) = self.lines.get(line).copied() else {
            // The scanner may emit an end marker on a synthetic row past EOF.
            let last = self.lines.len() - 1;
            return (last as u32, self.lines[last].len() as u32);
        };
        let col = self
            .unicode
            .get(&line)
            .map_or(mark.col().min(text.len()), |offsets| {
                offsets.get(mark.col()).copied().unwrap_or(text.len())
            });
        (line as u32, col as u32)
    }
}
fn read_node(
    events: &[(Event, Marker)],
    cursor: &mut usize,
    source: &SourceMap<'_>,
    anchors: &mut HashMap<usize, Span>,
    depth: usize,
) -> Result<Span, String> {
    if depth > 128 {
        return Err("YAML source map nesting limit exceeded".into());
    }
    let (event, start) = events
        .get(*cursor)
        .ok_or("Missing YAML node event")?
        .clone();
    *cursor += 1;
    let collection = matches!(event, Event::SequenceStart(..) | Event::MappingStart(..));
    let mut first_key = None;
    let (children, anchor, end) = match event {
        Event::Scalar(text, _, anchor, _) => {
            let end = events.get(*cursor).map_or(start, |e| e.1);
            (Children::Scalar(text), anchor, end)
        }
        Event::Alias(anchor) => {
            return anchors
                .get(&anchor)
                .cloned()
                .ok_or_else(|| "Unresolved YAML source anchor".into())
        }
        Event::SequenceStart(anchor, _) => {
            let mut items = Vec::new();
            while !matches!(events.get(*cursor), Some((Event::SequenceEnd, _))) {
                items.push(read_node(events, cursor, source, anchors, depth + 1)?);
            }
            let end = events[*cursor].1;
            *cursor += 1;
            (Children::Seq(items), anchor, end)
        }
        Event::MappingStart(anchor, _) => {
            let mut items = BTreeMap::new();
            while !matches!(events.get(*cursor), Some((Event::MappingEnd, _))) {
                let key = read_node(events, cursor, source, anchors, depth + 1)?;
                if items.is_empty() {
                    first_key = Some((key.position.start_line, key.position.start_col));
                }
                let Children::Scalar(key) = key.children else {
                    return Err("Non-scalar workflow key".into());
                };
                let value = read_node(events, cursor, source, anchors, depth + 1)?;
                items.insert(key, value);
            }
            let end = events[*cursor].1;
            *cursor += 1;
            (Children::Map(items), anchor, end)
        }
        _ => return Err("Unexpected YAML source event".into()),
    };
    let start_point = source.point(start);
    let (start_line, start_col) = first_key.map_or(start_point, |key| key.min(start_point));
    let (end_line, mut end_col) = source.point(end);
    if collection
        && source
            .lines
            .get(end_line as usize)
            .and_then(|line| line.get(end_col as usize..))
            .is_some_and(|rest| rest.starts_with('}') || rest.starts_with(']'))
    {
        end_col += 1;
    }
    let span = Span {
        position: Position {
            start_line,
            start_col,
            end_line,
            end_col,
        },
        children,
    };
    if anchor != 0 {
        anchors.insert(anchor, span.clone());
    }
    Ok(span)
}
fn at<'a>(mut span: &'a Span, path: &[&str]) -> Option<&'a Span> {
    for part in path {
        span = match &span.children {
            Children::Map(map) => map.get(*part)?,
            Children::Seq(seq) => seq.get(part.parse::<usize>().ok()?)?,
            Children::Scalar(_) => return None,
        };
    }
    Some(span)
}
fn assign(
    node: &mut SemanticNode,
    position: &mut impl FnMut(&[&str]) -> Option<Position>,
) -> Result<(), String> {
    let parts: Vec<_> = node.id.split('.').collect();
    let mut path = Vec::new();
    if parts.len() > 1 {
        path.push(match parts[1] {
            "task" => "tasks",
            "cluster" => "job_clusters",
            "param" => "parameters",
            _ => return Err("Unknown workflow source path".into()),
        });
        path.push(parts.get(2).ok_or("Missing workflow source index")?);
        if parts.len() > 3 {
            path.push(match parts[3] {
                "lib" => "libraries",
                "dep" => "depends_on",
                _ => return Err("Unknown task source path".into()),
            });
            path.push(parts.get(4).ok_or("Missing task source index")?);
        }
    }
    node.position = position(&path).ok_or("Missing workflow source span")?;
    for child in &mut node.children {
        assign(child, position)?;
    }
    Ok(())
}
pub(crate) fn attach(node: &mut SemanticNode, source: &str, is_json: bool) -> Result<(), String> {
    if is_json {
        let mut spans = crate::json_spans::JsonSpans::new(source)?;
        return assign(node, &mut |path| spans.position(path));
    }
    let mut events = Events(Vec::new());
    Parser::new_from_str(source)
        .load(&mut events, false)
        .map_err(|e| e.to_string())?;
    let mut cursor = events
        .0
        .iter()
        .position(|(e, _)| matches!(e, Event::MappingStart(..)))
        .ok_or("Missing workflow mapping")?;
    let root = read_node(
        &events.0,
        &mut cursor,
        &SourceMap::new(source),
        &mut HashMap::new(),
        0,
    )?;
    assign(node, &mut |path| {
        at(&root, path).map(|span| span.position.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Obtain real scanner markers without relying on a public Marker constructor.
    fn marker_at(row: usize, col: usize) -> Marker {
        let yaml = format!("{}{}[]", "\n".repeat(row), " ".repeat(col));
        let mut events = Events(Vec::new());
        Parser::new_from_str(&yaml)
            .load(&mut events, false)
            .unwrap();
        let mark = events
            .0
            .into_iter()
            .find_map(|(event, mark)| matches!(event, Event::SequenceStart(..)).then_some(mark))
            .unwrap();
        assert_eq!((mark.line(), mark.col()), (row + 1, col));
        mark
    }

    #[test]
    fn scanner_points_at_or_beyond_eof_use_the_exact_raw_source_end() {
        for (source, eof, eof_scalar_col) in [
            ("", (0, 0), 0),
            ("name: job\ntasks: []", (1, 9), 9),
            ("name: job\ntasks: []\n", (2, 0), 0),
            ("name: job\r\ntasks: []", (1, 9), 9),
            ("name: job\r\ntasks: []\r\n", (2, 0), 0),
            ("é雪🚀", (0, 9), 3),
            ("é\r\n雪", (1, 3), 1),
        ] {
            let map = SourceMap::new(source);
            assert_eq!(
                map.point(marker_at(eof.0 as usize, eof_scalar_col)),
                eof,
                "{source:?}"
            );
            assert_eq!(map.point(marker_at(eof.0 as usize, 100)), eof, "{source:?}");
            assert_eq!(map.point(marker_at(10, 0)), eof, "{source:?}");
            assert_eq!(map.point(marker_at(10, 100)), eof, "{source:?}");
        }
    }

    #[test]
    fn scanner_columns_map_to_raw_utf8_byte_boundaries_including_cr() {
        let map = SourceMap::new("é雪🚀\r\nβ");
        for (col, byte) in [(0, 0), (1, 2), (2, 5), (3, 9), (4, 10), (100, 10)] {
            assert_eq!(map.point(marker_at(0, col)), (0, byte));
        }
        assert_eq!(map.point(marker_at(1, 0)), (1, 0));
        assert_eq!(map.point(marker_at(1, 1)), (1, 2));
        assert_eq!(map.point(marker_at(1, 100)), (1, 2));
        let ascii = SourceMap::new("a\r\nb");
        assert_eq!(ascii.point(marker_at(0, 100)), (0, 2));
    }
}

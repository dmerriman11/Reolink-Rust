//! Read and change single elements of a camera's settings XML by path, leaving everything else byte-for-byte
//! intact. Settings are written read-modify-write so fields this app doesn't model are never dropped or reset.
//!
//! A path names elements below `<body>`, e.g. `VideoInput/bright` or `InputAdvanceCfg/DayNight/mode`.
//! A segment may pick the n-th (0-based) of same-named siblings: `MD/sensitivityInfoList/sensitivityInfo[2]/sensitivity`.

use quick_xml::events::{BytesText, Event};
use quick_xml::{Reader, Writer};
use std::collections::HashMap;

struct Seg<'a> {
    name: &'a str,
    index: Option<usize>,
}

fn parts(path: &str) -> Vec<Seg<'_>> {
    path.split('/')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('[') {
            Some((name, rest)) => Seg { name, index: rest.trim_end_matches(']').parse().ok() },
            None => Seg { name: p, index: None },
        })
        .collect()
}

/// Where the reader is: each open element with its index among same-named siblings.
#[derive(Default)]
struct Tracker {
    stack: Vec<(Vec<u8>, usize)>,
    // Per open element (plus the document root), how many children of each name have been seen.
    counts: Vec<HashMap<Vec<u8>, usize>>,
}

impl Tracker {
    fn open(&mut self, name: &[u8]) {
        if self.counts.is_empty() {
            self.counts.push(HashMap::new());
        }
        let seen = self.counts.last_mut().unwrap().entry(name.to_vec()).or_insert(0);
        let idx = *seen;
        *seen += 1;
        self.stack.push((name.to_vec(), idx));
        self.counts.push(HashMap::new());
    }

    fn close(&mut self) {
        self.stack.pop();
        self.counts.pop();
    }

    /// Does the current element match `want` (names below `<body>`, indices where given)?
    fn at(&self, want: &[Seg]) -> bool {
        let s = match self.stack.first() {
            Some((first, _)) if first == b"body" => &self.stack[1..],
            _ => &self.stack[..],
        };
        s.len() == want.len()
            && s.iter().zip(want).all(|((name, idx), w)| name == w.name.as_bytes() && w.index.is_none_or(|i| i == *idx))
    }
}

/// Text of the first element at `path`, or None when it doesn't exist.
pub fn get(xml: &str, path: &str) -> Option<String> {
    get_all(xml, path).into_iter().next()
}

/// Text of every element at `path`, in document order (e.g. each `<time>` of a list).
pub fn get_all(xml: &str, path: &str) -> Vec<String> {
    let want = parts(path);
    let mut reader = Reader::from_str(xml);
    let mut t = Tracker::default();
    let mut text = String::new();
    let mut out = vec![];
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                t.open(e.local_name().as_ref());
                text.clear();
            }
            Ok(Event::Empty(e)) => {
                t.open(e.local_name().as_ref());
                if t.at(&want) {
                    out.push(String::new());
                }
                t.close();
            }
            Ok(Event::Text(x)) => text.push_str(&x.unescape().unwrap_or_default()),
            Ok(Event::End(_)) => {
                if t.at(&want) {
                    out.push(text.trim().to_string());
                }
                t.close();
                text.clear();
            }
            Ok(Event::Eof) | Err(_) => return out,
            _ => {}
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EditError {
    #[error("setting {0} not found in the camera's reply")]
    NotFound(String),
    #[error("setting {0} is a group, not a value")]
    NotLeaf(String),
    #[error("camera XML could not be parsed: {0}")]
    Parse(String),
}

fn werr(e: impl std::fmt::Display) -> EditError {
    EditError::Parse(e.to_string())
}

/// `xml` with the text of the first element at `path` replaced by `value` (escaped).
pub fn set(xml: &str, path: &str, value: &str) -> Result<String, EditError> {
    let want = parts(path);
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Vec::with_capacity(xml.len() + value.len()));
    let mut t = Tracker::default();
    // While inside the target element: its text is replaced, and child elements make it an error.
    let mut in_target = false;
    let mut done = false;
    loop {
        let ev = reader.read_event().map_err(werr)?;
        match &ev {
            Event::Start(e) => {
                if in_target {
                    return Err(EditError::NotLeaf(path.into()));
                }
                t.open(e.local_name().as_ref());
                if !done && t.at(&want) {
                    in_target = true;
                    writer.write_event(ev.clone()).map_err(werr)?;
                    writer.write_event(Event::Text(BytesText::new(value))).map_err(werr)?;
                    continue;
                }
            }
            Event::Empty(e) => {
                t.open(e.local_name().as_ref());
                if !done && t.at(&want) {
                    // <x/> becomes <x>value</x>.
                    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    writer.write_event(Event::Start(e.to_owned())).map_err(werr)?;
                    writer.write_event(Event::Text(BytesText::new(value))).map_err(werr)?;
                    writer.write_event(Event::End(quick_xml::events::BytesEnd::new(name))).map_err(werr)?;
                    t.close();
                    done = true;
                    continue;
                }
                t.close();
            }
            Event::Text(_) | Event::CData(_) if in_target => continue, // old value dropped
            Event::End(_) => {
                if in_target {
                    in_target = false;
                    done = true;
                }
                t.close();
            }
            Event::Eof => break,
            _ => {}
        }
        writer.write_event(ev).map_err(werr)?;
    }
    if !done {
        return Err(EditError::NotFound(path.into()));
    }
    String::from_utf8(writer.into_inner()).map_err(|e| EditError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real replies from an Argus 4 Pro (msg 208 and a trimmed msg 26).
    const LED: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<LedState version=\"1.1\">\n<channelId>0</channelId>\n<ledVersion>0</ledVersion>\n<IRLedBrightness>80</IRLedBrightness>\n<state>close</state>\n<lightState>open</lightState>\n</LedState>\n</body>\n";
    const VIDEO: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<VideoInput version=\"1.1\">\n<channelId>0</channelId>\n<bright>128</bright>\n<contrast>128</contrast>\n</VideoInput>\n<InputAdvanceCfg version=\"1.1\">\n<PowerLineFrequency>\n<mode>60hz</mode>\n<enable>1</enable>\n</PowerLineFrequency>\n<DayNight>\n<mode>color</mode>\n</DayNight>\n<Scene>\n<mode>auto</mode>\n</Scene>\n</InputAdvanceCfg>\n</body>\n";
    const MD: &str = "<body><MD><sensitivityInfoList><sensitivityInfo><id>0</id><sensitivity>10</sensitivity></sensitivityInfo><sensitivityInfo><id>1</id><sensitivity>11</sensitivity></sensitivityInfo><sensitivityInfo><id>2</id><sensitivity>12</sensitivity></sensitivityInfo></sensitivityInfoList></MD></body>";

    #[test]
    fn reads_nested_paths() {
        assert_eq!(get(LED, "LedState/IRLedBrightness").as_deref(), Some("80"));
        assert_eq!(get(VIDEO, "InputAdvanceCfg/DayNight/mode").as_deref(), Some("color"));
        // Same leaf name under different parents is told apart by the full path.
        assert_eq!(get(VIDEO, "InputAdvanceCfg/Scene/mode").as_deref(), Some("auto"));
        assert_eq!(get(VIDEO, "VideoInput/missing"), None);
    }

    #[test]
    fn indexed_segments_pick_one_sibling() {
        assert_eq!(get(MD, "MD/sensitivityInfoList/sensitivityInfo[1]/sensitivity").as_deref(), Some("11"));
        assert_eq!(get(MD, "MD/sensitivityInfoList/sensitivityInfo[2]/id").as_deref(), Some("2"));
        assert_eq!(get(MD, "MD/sensitivityInfoList/sensitivityInfo[3]/sensitivity"), None);
        let out = set(MD, "MD/sensitivityInfoList/sensitivityInfo[1]/sensitivity", "40").unwrap();
        assert_eq!(get_all(&out, "MD/sensitivityInfoList/sensitivityInfo/sensitivity"), ["10", "40", "12"]);
        assert_eq!(out, MD.replace("<sensitivity>11<", "<sensitivity>40<"));
    }

    #[test]
    fn set_changes_only_the_target() {
        let out = set(LED, "LedState/IRLedBrightness", "55").unwrap();
        assert_eq!(out, LED.replace("<IRLedBrightness>80<", "<IRLedBrightness>55<"));
        let out = set(VIDEO, "InputAdvanceCfg/DayNight/mode", "auto").unwrap();
        assert_eq!(get(&out, "InputAdvanceCfg/DayNight/mode").as_deref(), Some("auto"));
        assert_eq!(get(&out, "InputAdvanceCfg/Scene/mode").as_deref(), Some("auto"));
        assert_eq!(out.len(), VIDEO.len() + 4 - 5); // "color" -> "auto", nothing else touched
    }

    #[test]
    fn set_escapes_and_reports_errors() {
        let out = set(LED, "LedState/state", "a<b&c").unwrap();
        assert!(out.contains("<state>a&lt;b&amp;c</state>"));
        assert_eq!(get(&out, "LedState/state").as_deref(), Some("a<b&c"));
        assert_eq!(set(LED, "LedState/nope", "1"), Err(EditError::NotFound("LedState/nope".into())));
        assert_eq!(set(VIDEO, "InputAdvanceCfg/DayNight", "1"), Err(EditError::NotLeaf("InputAdvanceCfg/DayNight".into())));
    }

    #[test]
    fn get_all_lists_repeated_elements() {
        let xml = "<body><RecordCfg><timeList><time>8</time><time>15</time><time>30</time></timeList></RecordCfg></body>";
        assert_eq!(get_all(xml, "RecordCfg/timeList/time"), ["8", "15", "30"]);
        assert!(get_all(xml, "RecordCfg/nope").is_empty());
    }

    #[test]
    fn set_fills_empty_elements() {
        let xml = "<body><Osd><name/></Osd></body>";
        let out = set(xml, "Osd/name", "Front").unwrap();
        assert_eq!(out, "<body><Osd><name>Front</name></Osd></body>");
    }
}

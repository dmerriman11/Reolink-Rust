//! Read and change single elements of a camera's settings XML by path, leaving everything else byte-for-byte
//! intact. Settings are written read-modify-write so fields this app doesn't model are never dropped or reset.
//!
//! A path names elements below `<body>`, e.g. `VideoInput/bright` or `InputAdvanceCfg/DayNight/mode`.

use quick_xml::events::{BytesText, Event};
use quick_xml::{Reader, Writer};

fn parts(path: &str) -> Vec<&str> {
    path.split('/').filter(|p| !p.is_empty()).collect()
}

/// Element names from below `<body>` down to the current element.
fn below_body(stack: &[Vec<u8>]) -> &[Vec<u8>] {
    match stack.first() {
        Some(first) if first == b"body" => &stack[1..],
        _ => stack,
    }
}

fn matches(stack: &[Vec<u8>], want: &[&str]) -> bool {
    let s = below_body(stack);
    s.len() == want.len() && s.iter().zip(want).all(|(a, b)| a == b.as_bytes())
}

/// Text of the first element at `path`, or None when it doesn't exist.
pub fn get(xml: &str, path: &str) -> Option<String> {
    let want = parts(path);
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Vec<u8>> = vec![];
    let mut text = String::new();
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => {
                stack.push(e.local_name().as_ref().to_vec());
                text.clear();
            }
            Event::Empty(e) => {
                stack.push(e.local_name().as_ref().to_vec());
                if matches(&stack, &want) {
                    return Some(String::new());
                }
                stack.pop();
            }
            Event::Text(t) => text.push_str(&t.unescape().ok()?),
            Event::End(_) => {
                if matches(&stack, &want) {
                    return Some(text.trim().to_string());
                }
                stack.pop();
                text.clear();
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// Text of every element at `path`, in document order (e.g. each `<time>` of a list).
pub fn get_all(xml: &str, path: &str) -> Vec<String> {
    let want = parts(path);
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Vec<u8>> = vec![];
    let mut text = String::new();
    let mut out = vec![];
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                stack.push(e.local_name().as_ref().to_vec());
                text.clear();
            }
            Ok(Event::Text(t)) => text.push_str(&t.unescape().unwrap_or_default()),
            Ok(Event::End(_)) => {
                if matches(&stack, &want) {
                    out.push(text.trim().to_string());
                }
                stack.pop();
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

/// `xml` with the text of the first element at `path` replaced by `value` (escaped).
pub fn set(xml: &str, path: &str, value: &str) -> Result<String, EditError> {
    let want = parts(path);
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Vec::with_capacity(xml.len() + value.len()));
    let mut stack: Vec<Vec<u8>> = vec![];
    // While inside the target element: its text is replaced, and child elements make it an error.
    let mut in_target = false;
    let mut done = false;
    let err = |e: quick_xml::Error| EditError::Parse(e.to_string());
    loop {
        let ev = reader.read_event().map_err(err)?;
        match &ev {
            Event::Start(e) => {
                if in_target {
                    return Err(EditError::NotLeaf(path.into()));
                }
                stack.push(e.local_name().as_ref().to_vec());
                if !done && matches(&stack, &want) {
                    in_target = true;
                    writer.write_event(ev.clone()).map_err(|e| EditError::Parse(e.to_string()))?;
                    writer.write_event(Event::Text(BytesText::new(value))).map_err(|e| EditError::Parse(e.to_string()))?;
                    continue;
                }
            }
            Event::Empty(e) => {
                stack.push(e.local_name().as_ref().to_vec());
                if !done && matches(&stack, &want) {
                    // <x/> becomes <x>value</x>.
                    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    let start = e.to_owned();
                    writer.write_event(Event::Start(start)).map_err(|e| EditError::Parse(e.to_string()))?;
                    writer.write_event(Event::Text(BytesText::new(value))).map_err(|e| EditError::Parse(e.to_string()))?;
                    writer
                        .write_event(Event::End(quick_xml::events::BytesEnd::new(name)))
                        .map_err(|e| EditError::Parse(e.to_string()))?;
                    stack.pop();
                    done = true;
                    continue;
                }
                stack.pop();
            }
            Event::Text(_) | Event::CData(_) if in_target => continue, // old value dropped
            Event::End(_) => {
                if in_target {
                    in_target = false;
                    done = true;
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        writer.write_event(ev).map_err(|e| EditError::Parse(e.to_string()))?;
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

    #[test]
    fn reads_nested_paths() {
        assert_eq!(get(LED, "LedState/IRLedBrightness").as_deref(), Some("80"));
        assert_eq!(get(VIDEO, "InputAdvanceCfg/DayNight/mode").as_deref(), Some("color"));
        // Same leaf name under different parents is told apart by the full path.
        assert_eq!(get(VIDEO, "InputAdvanceCfg/Scene/mode").as_deref(), Some("auto"));
        assert_eq!(get(VIDEO, "VideoInput/missing"), None);
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

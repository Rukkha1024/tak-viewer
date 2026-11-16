//! `MetaData.dat` properties. Port of `scripts/marker_store_remote.py` `decode_marker_streams`
//! (~413-432): find `<properties>`...`</properties>` in UTF-16LE, then UTF-8; name/value are
//! child elements or attributes. Python `int()` / `float()` parsing rules for the frame fields.

use super::container::{find, utf16le_strict};
use super::{err, Result, TakError};

fn encode(text: &str, utf16: bool) -> Vec<u8> {
    if utf16 {
        text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    } else {
        text.as_bytes().to_vec()
    }
}

/// ElementTree `.text`: character data before the first child element (comments/PIs dropped).
fn et_text(node: roxmltree::Node) -> Option<String> {
    let mut out: Option<String> = None;
    for child in node.children() {
        if child.is_element() {
            break;
        }
        if child.is_text() {
            out.get_or_insert_with(String::new).push_str(child.text().unwrap_or(""));
        }
    }
    out
}

fn is_named(node: &roxmltree::Node, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.tag_name().name() == name
}

/// `p.findtext(key) if p.find(key) is not None else p.get(key)`.
fn field(p: &roxmltree::Node, key: &str) -> Option<String> {
    match p.children().find(|c| is_named(c, key)) {
        Some(child) => Some(et_text(child).unwrap_or_default()),
        None => p.attribute(key).map(str::to_string),
    }
}

/// Properties in file order; a repeated name keeps its first position and takes the last
/// value (Python dict). Missing values become "" (Python `None`); missing names are skipped.
pub fn parse_properties(meta: &[u8]) -> Result<Vec<(String, String)>> {
    let mut xml: Option<String> = None;
    for utf16 in [true, false] {
        let opening = encode("<properties>", utf16);
        let closing = encode("</properties>", utf16);
        let Some(start) = find(meta, &opening, 0) else { continue };
        let Some(end) = find(meta, &closing, start) else { continue };
        let slice = &meta[start..end + closing.len()];
        let text = if utf16 {
            utf16le_strict(slice)?
        } else {
            std::str::from_utf8(slice).map_err(|e| TakError(format!("Metadata XML not UTF-8: {e}")))?.to_string()
        };
        xml = Some(text);
        break;
    }
    let Some(xml) = xml else { return err("Metadata XML missing") };
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(&xml, options)
        .map_err(|e| TakError(format!("Metadata XML invalid: {e}")))?;
    let mut out: Vec<(String, String)> = Vec::new();
    for p in doc.root_element().children().filter(|c| is_named(c, "property")) {
        let Some(name) = field(&p, "name") else { continue };
        let value = field(&p, "value").unwrap_or_default();
        match out.iter_mut().find(|(k, _)| *k == name) {
            Some(slot) => slot.1 = value,
            None => out.push((name, value)),
        }
    }
    Ok(out)
}

/// Remove underscores that Python allows between digits; reject any other underscore.
fn strip_digit_underscores(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, ch) in s.char_indices() {
        if ch == '_' {
            let prev_ok = i > 0 && b[i - 1].is_ascii_digit();
            let next_ok = i + 1 < b.len() && b[i + 1].is_ascii_digit();
            if !(prev_ok && next_ok) {
                return None;
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

/// Python `int(str)` for base-10 text (whitespace, sign, digit underscores).
pub fn python_int(s: &str) -> Result<i64> {
    let bad = || TakError(format!("invalid literal for int() with base 10: {s:?}"));
    let t = s.trim();
    let digits = strip_digit_underscores(t).ok_or_else(bad)?;
    let body = digits.strip_prefix(['+', '-']).unwrap_or(&digits);
    if body.is_empty() || !body.bytes().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    digits.parse::<i64>().map_err(|_| TakError(format!("integer out of supported range: {s:?}")))
}

/// Python `float(str)`.
pub fn python_float(s: &str) -> Result<f64> {
    let bad = || TakError(format!("could not convert string to float: {s:?}"));
    let t = s.trim();
    let cleaned = strip_digit_underscores(t).ok_or_else(bad)?;
    let body = cleaned.strip_prefix(['+', '-']).unwrap_or(&cleaned);
    let lower = body.to_ascii_lowercase();
    let word = matches!(lower.as_str(), "inf" | "infinity" | "nan");
    if !word && !body.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-')) {
        return Err(bad());
    }
    cleaned.parse::<f64>().map_err(|_| bad())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        encode(s, true)
    }

    #[test]
    fn utf16_properties_children_and_attributes() {
        let mut raw = vec![1u8, 2, 3];
        raw.extend(utf16(
            "<properties><property><name>StartFrame</name><value>5</value></property>\
             <property name=\"FrameRate\" value=\"120\"/><property><name>Empty</name></property>\
             <property><name>StartFrame</name><value>7</value></property></properties>",
        ));
        raw.extend([0, 0]);
        let props = parse_properties(&raw).unwrap();
        assert_eq!(
            props,
            vec![
                ("StartFrame".to_string(), "7".to_string()),
                ("FrameRate".to_string(), "120".to_string()),
                ("Empty".to_string(), "".to_string()),
            ]
        );
    }

    #[test]
    fn utf8_fallback_and_missing() {
        let raw = b"xx<properties><property><name>A</name><value>b</value></property></properties>";
        assert_eq!(parse_properties(raw).unwrap(), vec![("A".to_string(), "b".to_string())]);
        assert!(parse_properties(b"nothing here").is_err());
    }

    #[test]
    fn python_number_rules() {
        assert_eq!(python_int(" 12 ").unwrap(), 12);
        assert_eq!(python_int("+1_000").unwrap(), 1000);
        assert!(python_int("1.0").is_err());
        assert!(python_int("").is_err());
        assert!(python_int("_1").is_err());
        assert_eq!(python_float("120").unwrap(), 120.0);
        assert_eq!(python_float(" 1e2 ").unwrap(), 100.0);
        assert!(python_float("abc").is_err());
        assert!(python_float("inf").unwrap().is_infinite());
    }
}

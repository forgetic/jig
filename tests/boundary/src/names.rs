//! Scan source names and comments while leaving quoted data with its owner.

use std::path::Path;

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 128
}

fn forbidden_name(name: &str) -> bool {
    let stem = name.trim_end_matches(|ch: char| ch.is_ascii_digit());
    if stem.len() < name.len() && (stem.ends_with('V') || stem.ends_with("_v")) {
        return true;
    }
    let bytes = name.as_bytes();
    let mut start = 0;
    for end in 0..=bytes.len() {
        let boundary = end == bytes.len()
            || bytes[end] == b'_'
            || (end > start
                && bytes[end].is_ascii_uppercase()
                && (bytes[end - 1].is_ascii_lowercase() || bytes.get(end + 1).is_some_and(u8::is_ascii_lowercase)))
            || (end > start && bytes[end].is_ascii_digit());
        if boundary {
            let part = &name[start..end];
            if matches!(part.len(), 5..=7)
                && ["Typed", "Untyped", "Legacy"].iter().any(|word| part.eq_ignore_ascii_case(word))
            {
                return true;
            }
            start = if bytes.get(end) == Some(&b'_') { end + 1 } else { end };
        }
    }
    false
}

fn quoted_end(source: &[u8], quote: usize) -> usize {
    let mut end = quote + 1;
    while end < source.len() {
        match source[end] {
            b'\\' => end += 2,
            b'"' => return end + 1,
            _ => end += 1,
        }
    }
    source.len()
}

fn literal_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut quote = start;
    if bytes[quote] == b'b' || bytes[quote] == b'c' {
        quote += 1;
    }
    if bytes.get(quote) == Some(&b'r') {
        let mut body = quote + 1;
        while bytes.get(body) == Some(&b'#') {
            body += 1;
        }
        if bytes.get(body) != Some(&b'"') {
            return None;
        }
        let hashes = body - quote - 1;
        for (offset, _) in source[body + 1..].match_indices('"') {
            let end = body + 2 + offset;
            if bytes.get(end..end + hashes).is_some_and(|tail| tail.iter().all(|byte| *byte == b'#')) {
                return Some(end + hashes);
            }
        }
        return Some(bytes.len());
    }
    match bytes.get(quote) {
        Some(b'"') => Some(quoted_end(bytes, quote)),
        Some(b'\'') => {
            let mut end = quote + 1;
            let ch = source.get(end..)?.chars().next()?;
            end += ch.len_utf8();
            if ch == '\\' {
                if bytes.get(end) == Some(&b'u') {
                    end = source[end..].find('}').map(|offset| end + offset + 1)?;
                } else if bytes.get(end) == Some(&b'x') {
                    end += 3;
                } else {
                    end += 1;
                }
            }
            (bytes.get(end) == Some(&b'\'')).then_some(end + 1)
        }
        _ => None,
    }
}

fn comment_end(source: &[u8], start: usize) -> usize {
    if source[start + 1] == b'/' {
        return source[start..].iter().position(|byte| *byte == b'\n').map_or(source.len(), |end| start + end);
    }
    let mut depth = 1;
    let mut end = start + 2;
    while end < source.len() && depth != 0 {
        match source.get(end..end + 2) {
            Some(b"/*") => {
                depth += 1;
                end += 2;
            }
            Some(b"*/") => {
                depth -= 1;
                end += 2;
            }
            _ => end += 1,
        }
    }
    end
}

fn has_candidate(source: &str, lower: &str) -> bool {
    lower.contains("typed")
        || lower.contains("legacy")
        || source.match_indices('V').any(|(at, _)| source.as_bytes().get(at + 1).is_some_and(u8::is_ascii_digit))
        || lower.match_indices("_v").any(|(at, _)| lower.as_bytes().get(at + 2).is_some_and(u8::is_ascii_digit))
}

pub(super) fn breaches(path: &Path, source: &str, lower: &str) -> Vec<String> {
    let mut failures = Vec::new();
    if !has_candidate(source, lower) {
        return failures;
    }
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut line = 1;
    while index < bytes.len() {
        let start = index;
        let literal =
            if matches!(bytes[index], b'b' | b'c' | b'r' | b'"' | b'\'') { literal_end(source, index) } else { None };
        if let Some(end) = literal {
            index = end;
        } else if matches!(bytes.get(index..index + 2), Some(b"//" | b"/*")) {
            index = comment_end(bytes, index);
            for (offset, byte) in bytes[start..index].iter().enumerate() {
                if !matches!(byte, b'l' | b'L') {
                    continue;
                }
                let at = start + offset;
                if bytes.get(at..at + 6).is_some_and(|word| word.eq_ignore_ascii_case(b"legacy"))
                    && !at
                        .checked_sub(1)
                        .and_then(|before| bytes.get(before))
                        .is_some_and(|byte| identifier_byte(*byte))
                    && !bytes.get(at + 6).is_some_and(|byte| identifier_byte(*byte))
                {
                    let comment_line = line + source[start..at].matches('\n').count();
                    let word = &source[at..at + 6];
                    failures.push(format!("{}:{comment_line}: comment {word}", path.display()));
                }
            }
        } else if identifier_byte(bytes[index]) && !bytes[index].is_ascii_digit() {
            index += 1;
            while bytes.get(index).is_some_and(|byte| identifier_byte(*byte)) {
                index += 1;
            }
            let name = &source[start..index];
            if forbidden_name(name) {
                failures.push(format!("{}:{line}: identifier {name}", path.display()));
            }
        } else {
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
            }
        }
        line += source[start..index].matches('\n').count();
    }
    failures
}

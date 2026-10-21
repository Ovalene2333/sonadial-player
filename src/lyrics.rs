use std::path::Path;

#[derive(Clone, Debug)]
pub struct LyricLine {
    pub time: f64,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedLyrics {
    pub lines: Vec<LyricLine>,
    /// False for plain-text lyrics without any `[mm:ss.xx]` timestamps.
    pub synced: bool,
}

/// Load an LRC file. UTF-8 (with or without BOM) and UTF-16 BOM files are
/// supported; other encodings fall back to a lossy UTF-8 read.
pub fn load(path: &Path) -> ParsedLyrics {
    let Ok(bytes) = std::fs::read(path) else {
        return ParsedLyrics::default();
    };
    parse(&decode(&bytes))
}

fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units = rest
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn parse(text: &str) -> ParsedLyrics {
    let mut offset_ms = 0.0_f64;
    let mut out = Vec::new();
    let mut plain = Vec::new();

    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim_end();
        if line.is_empty() {
            continue;
        }

        let mut times = Vec::new();
        let mut rest = line;
        loop {
            let trimmed = rest.trim_start();
            if !trimmed.starts_with('[') {
                rest = trimmed;
                break;
            }
            let Some(end) = trimmed.find(']') else {
                rest = trimmed;
                break;
            };
            let tag = &trimmed[1..end];
            if let Some(offset) = parse_offset(tag) {
                offset_ms = offset;
            } else if let Some(time) = parse_time(tag) {
                times.push(time);
            }
            rest = &trimmed[end + 1..];
        }

        let text = rest.trim();
        if text.is_empty() {
            continue;
        }
        if times.is_empty() {
            plain.push(text.to_owned());
        } else {
            for time in times {
                out.push(LyricLine {
                    time: (time - offset_ms / 1000.0).max(0.0),
                    text: text.to_owned(),
                });
            }
        }
    }

    out.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if out.is_empty() {
        return ParsedLyrics {
            lines: plain
                .into_iter()
                .map(|text| LyricLine { time: 0.0, text })
                .collect(),
            synced: false,
        };
    }
    ParsedLyrics {
        lines: out,
        synced: true,
    }
}

/// Index of the line that should be visible at `position`; `None` before the
/// first timestamp.
pub fn active_index(lines: &[LyricLine], position: f64) -> Option<usize> {
    let count = lines.partition_point(|line| line.time <= position + 0.03);
    count.checked_sub(1)
}

fn parse_offset(tag: &str) -> Option<f64> {
    let (key, value) = tag.split_once(':')?;
    if !key.trim().eq_ignore_ascii_case("offset") {
        return None;
    }
    value.trim().parse::<f64>().ok()
}

fn parse_time(tag: &str) -> Option<f64> {
    let mut parts = tag.split(':');
    let first = parts.next()?.trim().parse::<f64>().ok()?;
    let second = parts.next()?.trim().replace(',', ".");
    let second = second.parse::<f64>().ok()?;

    if let Some(third) = parts.next() {
        let third = third.trim().replace(',', ".");
        let third = third.parse::<f64>().ok()?;
        // Common LRC variant: [mm:ss:cs]. Treat the final field as
        // centiseconds, not as a third clock component.
        let fraction = if third >= 100.0 { 0.0 } else { third / 100.0 };
        Some(first * 60.0 + second + fraction)
    } else {
        Some(first * 60.0 + second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_timestamps_and_offset() {
        let lyrics =
            parse("[ar:test]\n[offset:-200]\n[00:01.50][00:03.00]Hello\n[00:02.25]World\n");
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines.len(), 3);
        assert!((lyrics.lines[0].time - 1.7).abs() < 1e-6);
        assert!((lyrics.lines[1].time - 2.45).abs() < 1e-6);
        assert!((lyrics.lines[2].time - 3.2).abs() < 1e-6);
    }

    #[test]
    fn finds_active_line() {
        let lyrics = parse("[00:01.00]one\n[00:02.00]two\n");
        assert_eq!(active_index(&lyrics.lines, 0.5), None);
        assert_eq!(active_index(&lyrics.lines, 1.2), Some(0));
        assert_eq!(active_index(&lyrics.lines, 9.0), Some(1));
    }

    #[test]
    fn keeps_plain_lyrics_as_unsynced() {
        let lyrics = parse("first line\nsecond line\n");
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[1].text, "second line");
    }
}

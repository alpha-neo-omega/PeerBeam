//! Clipboard entity for cross-device clipboard sync.
//!
//! Handles the four content classes PeerBeam shares — plain **text**, **URLs**,
//! **code**, and **images** — with a payload that is either UTF-8 text or raw
//! bytes. [`classify`] labels text automatically so the receiver can present
//! it correctly (open a link, syntax-highlight code, …).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of clipboard content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardKind {
    /// Plain text.
    Text,
    /// A single URL.
    Url,
    /// A source-code snippet.
    Code,
    /// A raster image (payload is bytes; see `mime`).
    Image,
}

/// The clipboard payload: UTF-8 text or raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardData {
    /// Textual content (text/url/code).
    Text(String),
    /// Binary content (images).
    Bytes(Vec<u8>),
}

/// A clipboard item that can be shared between devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardItem {
    /// The content class.
    pub kind: ClipboardKind,
    /// MIME type (e.g. `text/plain`, `text/uri-list`, `image/png`).
    pub mime: String,
    /// The payload.
    pub data: ClipboardData,
    /// When the item was captured.
    pub at: DateTime<Utc>,
}

impl ClipboardItem {
    /// Build a text item, auto-classifying it as text, URL, or code.
    pub fn text(content: String, at: DateTime<Utc>) -> Self {
        let kind = classify(&content);
        let mime = match kind {
            ClipboardKind::Url => "text/uri-list",
            _ => "text/plain",
        }
        .to_string();
        Self {
            kind,
            mime,
            data: ClipboardData::Text(content),
            at,
        }
    }

    /// Build an image item from raw bytes and a MIME type.
    pub fn image(bytes: Vec<u8>, mime: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            kind: ClipboardKind::Image,
            mime: mime.into(),
            data: ClipboardData::Bytes(bytes),
            at,
        }
    }

    /// The text payload, if this item is textual.
    pub fn as_text(&self) -> Option<&str> {
        match &self.data {
            ClipboardData::Text(t) => Some(t),
            ClipboardData::Bytes(_) => None,
        }
    }

    /// The byte payload, if this item is binary.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &self.data {
            ClipboardData::Bytes(b) => Some(b),
            ClipboardData::Text(_) => None,
        }
    }
}

/// Classify a piece of text as a URL, code, or plain text.
///
/// Heuristic and intentionally conservative: a single whitespace-free `http`,
/// `https`, `ftp`, or `mailto` string is a URL; text with code markers (or
/// multiple lines plus one marker) is code; everything else is plain text.
pub fn classify(text: &str) -> ClipboardKind {
    let trimmed = text.trim();
    if is_url(trimmed) {
        ClipboardKind::Url
    } else if looks_like_code(text) {
        ClipboardKind::Code
    } else {
        ClipboardKind::Text
    }
}

fn is_url(s: &str) -> bool {
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return false;
    }
    ["http://", "https://", "ftp://", "mailto:"]
        .iter()
        .any(|p| s.starts_with(p))
}

fn looks_like_code(s: &str) -> bool {
    const MARKERS: &[&str] = &[
        "{",
        "}",
        ";",
        "=>",
        "def ",
        "fn ",
        "function ",
        "class ",
        "import ",
        "#include",
        "public ",
        "const ",
        "let ",
        "return ",
        "</",
    ];
    let hits = MARKERS.iter().filter(|m| s.contains(**m)).count();
    let multiline = s.lines().count() > 1;
    (multiline && hits >= 1) || hits >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn classifies_urls() {
        assert_eq!(classify("https://example.com/x"), ClipboardKind::Url);
        assert_eq!(classify("  http://a.b  "), ClipboardKind::Url);
        assert_eq!(classify("mailto:me@x.com"), ClipboardKind::Url);
        // Not a URL: has spaces.
        assert_eq!(classify("see https://x.com now"), ClipboardKind::Text);
    }

    #[test]
    fn classifies_code() {
        assert_eq!(
            classify("fn main() {\n    println!(\"hi\");\n}"),
            ClipboardKind::Code
        );
        assert_eq!(
            classify("import os\nprint(os.getcwd())"),
            ClipboardKind::Code,
            "import marker on a multi-line snippet → code"
        );
        assert_eq!(classify("const x = 1; let y = 2;"), ClipboardKind::Code);
    }

    #[test]
    fn classifies_plain_text() {
        assert_eq!(classify("just a normal sentence"), ClipboardKind::Text);
        assert_eq!(classify("hello world"), ClipboardKind::Text);
    }

    #[test]
    fn text_constructor_sets_kind_and_mime() {
        let url = ClipboardItem::text("https://x.com".into(), t0());
        assert_eq!(url.kind, ClipboardKind::Url);
        assert_eq!(url.mime, "text/uri-list");
        assert_eq!(url.as_text(), Some("https://x.com"));
        assert_eq!(url.as_bytes(), None);
    }

    #[test]
    fn image_constructor() {
        let img = ClipboardItem::image(vec![1, 2, 3], "image/png", t0());
        assert_eq!(img.kind, ClipboardKind::Image);
        assert_eq!(img.mime, "image/png");
        assert_eq!(img.as_bytes(), Some(&[1u8, 2, 3][..]));
        assert_eq!(img.as_text(), None);
    }
}

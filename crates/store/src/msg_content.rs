//! Message content: deciding whether stored text is structured content or a
//! plain string.
//!
//! Ported from `_parse_msg_content` (`core/session_manager.py:39-64`).
//!
//! # The predicate, and why it is this fussy
//!
//! The obvious reading — "if it parses as JSON, it's structure" — is wrong, and
//! porting it would destroy user data. Message content is untrusted text that
//! arrives from a chat box; a user pasting an API schema, a JSON sample or a
//! `curl` example must come back out of the database byte-identical. The Python
//! guards that with four separate conditions, and **all** of them are load
//! bearing:
//!
//! 1. the text starts with `[{` — an object (`{"a": 1}`) or a scalar is never
//!    multimodal content,
//! 2. the text contains the literal `"type"` — a cheap pre-filter that avoids
//!    parsing arbitrary text,
//! 3. it parses as JSON,
//! 4. every element is an object whose `type` is a recognised content-block
//!    kind.
//!
//! If **any** step fails the original string is returned untouched. That
//! fallback is the whole point of the function, so it is reproduced exactly
//! rather than improved: a stricter parser here would silently mangle a class
//! of message the Python handles correctly.
//!
//! The Python's own comment records the bug this guards against: a plain text
//! message that merely *looks* like a JSON array of objects was "silently parsed
//! back into a list, destroying the original string".

use std::fmt;

use serde_json::Value;

/// The content-block kinds the Python recognises (`session_manager.py:53-55`).
///
/// Mirrored verbatim. A block whose `type` is not in this set is not multimodal
/// content, and the message is left as a string.
pub const BLOCK_TYPES: [&str; 8] = [
    "text",
    "image",
    "image_url",
    "audio",
    "input_audio",
    "input_image",
    "document",
    "file",
];

/// Guard applied before parsing: `raw.startswith('[{')`.
const STRUCTURED_PREFIX: &str = "[{";
/// Guard applied before parsing: `'"type"' in raw`.
///
/// The quotes are part of the literal — it looks for the JSON key spelling, not
/// the word `type` appearing in prose.
const TYPE_KEY: &str = "\"type\"";

/// A parsed message body.
#[derive(Debug, Clone, PartialEq)]
pub enum MsgContent {
    /// A plain string, stored and returned verbatim.
    ///
    /// `Box<str>`, not `String`: a stored value with no capacity field that
    /// means anything after the fact (AGENTS.md §12.1 rule 1).
    Text(Box<str>),
    /// Multimodal content blocks, each kept as the JSON object it was.
    ///
    /// The blocks stay [`Value`] rather than gaining a typed struct on purpose:
    /// the Python returns the parsed dictionaries unchanged, and a typed
    /// `ContentBlock` would have to invent a representation for the keys this
    /// function does not inspect. Inventing one is how a port starts dropping
    /// fields.
    Blocks(Box<[Value]>),
}

impl MsgContent {
    /// Whether this is structured multimodal content rather than a string.
    #[must_use]
    pub fn is_blocks(&self) -> bool {
        matches!(self, Self::Blocks(_))
    }

    /// The blocks, if this is structured content.
    #[must_use]
    pub fn blocks(&self) -> Option<&[Value]> {
        match self {
            Self::Text(_) => None,
            Self::Blocks(b) => Some(b),
        }
    }

    /// The content as text, whatever the variant.
    ///
    /// For [`MsgContent::Blocks`] this is a lossy JSON rendering, provided for
    /// callers that only want something to display. It is not a round-trip, and
    /// is not a substitute for [`MsgContent::blocks`].
    #[must_use]
    pub fn to_text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Text(t) => std::borrow::Cow::Borrowed(t),
            Self::Blocks(_) => std::borrow::Cow::Owned(self.blocks_to_json()),
        }
    }

    /// A JSON array rendering of the blocks, for display.
    fn blocks_to_json(&self) -> String {
        let Self::Blocks(blocks) = self else {
            return String::new();
        };
        let mut out = String::from("[");
        for (i, block) in blocks.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&block.to_string());
        }
        out.push(']');
        out
    }
}

impl fmt::Display for MsgContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

/// Decide whether `raw` is multimodal content or a plain string.
///
/// A faithful port of `_parse_msg_content` (`session_manager.py:39-64`).
///
/// # Examples
///
/// ```
/// use odysseus_store::msg_content::parse_msg_content;
///
/// // A user pasting a JSON object: stays a string.
/// assert!(!parse_msg_content(r#"{"a": 1}"#).is_blocks());
///
/// // Genuine multimodal content: becomes blocks.
/// assert!(parse_msg_content(r#"[{"type": "text", "text": "hi"}]"#).is_blocks());
///
/// // An unknown `type` is not a content block: the string survives.
/// assert!(!parse_msg_content(r#"[{"type": "widget", "id": 7}]"#).is_blocks());
/// ```
#[must_use]
pub fn parse_msg_content(raw: &str) -> MsgContent {
    let text = || MsgContent::Text(raw.into());

    // Guard 1 and 2: the cheap pre-checks. A message failing either is never a
    // candidate, and we avoid the parse entirely.
    if !raw.starts_with(STRUCTURED_PREFIX) || !raw.contains(TYPE_KEY) {
        return text();
    }

    // Guard 3: does it parse as JSON *as an array*?
    //
    // `Vec<Value>` rather than `Value` so that a valid JSON object or scalar is
    // rejected here, matching Python's `isinstance(parsed, list)` check.
    let Ok(parsed) = serde_json::from_str::<Vec<Value>>(raw) else {
        return text();
    };

    // Python's `parsed and all(...)`: an empty list is falsy, so it takes the
    // same fallback. It cannot reach here through the `[{` prefix, but the
    // check is part of the predicate being ported.
    if parsed.is_empty() {
        return text();
    }

    // Guard 4: every element must be an object with a recognised `type`.
    if !parsed.iter().all(is_content_block) {
        return text();
    }

    MsgContent::Blocks(parsed.into_boxed_slice())
}

/// One element of the candidate array, per the Python's inline predicate:
/// `isinstance(p, dict) and p.get("type") in _BLOCK_TYPES`.
///
/// `Value::get` returns `None` for a missing key and for a non-object, so this
/// covers both halves of the Python's `and`.
fn is_content_block(value: &Value) -> bool {
    value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| BLOCK_TYPES.contains(&kind))
}

#[cfg(test)]
mod tests {
    use super::{MsgContent, Value, parse_msg_content};

    /// The awkward case the port exists for: a user typed valid JSON, and it
    /// must still come back as the exact string they typed.
    #[test]
    fn a_typed_json_object_stays_a_string() {
        let raw = r#"{"a": 1}"#;
        let parsed = parse_msg_content(raw);
        assert_eq!(parsed, MsgContent::Text(raw.into()));
        assert_eq!(parsed.to_text(), raw, "the string must be byte-identical");
    }

    #[test]
    fn an_array_whose_type_is_unknown_stays_a_string() {
        // Passes the `[{` and `"type"` guards and parses, but `widget` is not a
        // content-block kind, so the Python returns the original string.
        let raw = r#"[{"type": "widget", "id": 7}]"#;
        assert_eq!(parse_msg_content(raw), MsgContent::Text(raw.into()));
    }

    #[test]
    fn an_array_without_a_type_key_stays_a_string() {
        let raw = r#"[{"id": 7}]"#;
        assert_eq!(parse_msg_content(raw), MsgContent::Text(raw.into()));
    }

    #[test]
    fn an_array_with_one_good_and_one_bad_element_stays_a_string() {
        // `all(...)`, not `any(...)`: one unrecognised element is enough.
        let raw = r#"[{"type": "text"}, {"type": "widget"}]"#;
        assert_eq!(parse_msg_content(raw), MsgContent::Text(raw.into()));
    }

    #[test]
    fn a_non_object_element_stays_a_string() {
        let raw = r#"[{"type": "text"}, "loose string"]"#;
        assert_eq!(parse_msg_content(raw), MsgContent::Text(raw.into()));
    }

    #[test]
    fn a_non_string_type_value_stays_a_string() {
        // `p.get("type")` returns 7 here, which is not in the block types.
        let raw = r#"[{"type": 7}]"#;
        assert_eq!(parse_msg_content(raw), MsgContent::Text(raw.into()));
    }

    #[test]
    fn text_that_merely_contains_the_word_type_stays_a_string() {
        // The guard looks for the quoted key `"type"`, so prose is untouched.
        let raw = "please explain the type of this error";
        assert_eq!(parse_msg_content(raw), MsgContent::Text(raw.into()));
    }

    #[test]
    fn genuine_multimodal_content_becomes_blocks() {
        let parsed = parse_msg_content(
            r#"[{"type": "text", "text": "hi"}, {"type": "image_url", "image_url": {"url": "x"}}]"#,
        );
        assert!(parsed.is_blocks());
        assert_eq!(parsed.blocks().map(<[Value]>::len), Some(2));
    }

    #[test]
    fn every_recognised_block_type_is_accepted() {
        for kind in super::BLOCK_TYPES {
            let raw = format!(r#"[{{"type": "{kind}"}}]"#);
            assert!(
                parse_msg_content(&raw).is_blocks(),
                "{kind} is in BLOCK_TYPES and must be accepted"
            );
        }
    }

    #[test]
    fn extra_keys_on_a_block_survive() {
        // The Python returns the parsed dict unchanged. A port that mapped
        // blocks onto a typed struct holding only `type` would drop `detail`.
        let raw = r#"[{"type": "text", "text": "hi", "detail": {"nested": [1, 2]}}]"#;
        let parsed = parse_msg_content(raw);
        let blocks = parsed.blocks().unwrap_or_default();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks
                .first()
                .and_then(|b| b.get("detail"))
                .map(Value::to_string),
            Some(r#"{"nested":[1,2]}"#.to_string())
        );
    }

    #[test]
    fn unparseable_text_that_passes_the_guards_stays_a_string() {
        // Starts with `[{`, contains `"type"`, and is not valid JSON.
        let raw = r#"[{"type": "text", oops"#;
        assert_eq!(parse_msg_content(raw), MsgContent::Text(raw.into()));
    }

    #[test]
    fn an_empty_array_is_not_treated_as_content() {
        assert!(!parse_msg_content("[]").is_blocks());
    }

    #[test]
    fn plain_prose_is_untouched() {
        for raw in ["", "hello", "a { b } c", "  ", "[not json"] {
            assert_eq!(
                parse_msg_content(raw),
                MsgContent::Text(raw.into()),
                "{raw:?} must round-trip as a string"
            );
        }
    }
}

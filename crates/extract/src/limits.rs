/// Configurable limits for hostile-content parsing per MIME-AND-TEXT.md §13.

/// Compile‑time constants used throughout the codebase. These are public so other crates can enforce the same limits without needing to instantiate `Limits` at runtime.
pub const MAX_RAW_MESSAGE_SIZE: usize = 25 * 1024 * 1024; // 25 MiB
pub const MAX_MIME_NESTING_DEPTH: usize = 20;
pub const MAX_PARTS_PER_MESSAGE: usize = 500;
pub const MAX_EXTRACTED_BODY_SIZE: usize = 1024 * 1024; // 1 MiB
pub const MAX_ATTACHMENT_COUNT: usize = 100;
pub const MAX_HEADER_LINE_LENGTH: usize = 8 * 1024; // 8 KiB
pub const MAX_SINGLE_HEADER_VALUE: usize = 64 * 1024; // 64 KiB
pub const HTML_PARSER_NODE_BUDGET: usize = 100_000;
pub const FEED_RESPONSE_SIZE_LIMIT: usize = 10 * 1024 * 1024; // 10 MiB

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Max raw message size before aborting body parse (default 25 MB).
    pub max_raw_message_size: usize,
    /// Max MIME nesting depth before recursion stops (default 20).
    pub max_mime_nesting_depth: usize,
    /// Max total parts per message before remaining are ignored (default 500).
    pub max_parts_per_message: usize,
    /// Max extracted body size in bytes before truncating at paragraph boundary (default 1 MB).
    pub max_extracted_body_size: usize,
    /// Max attachment parts recorded before remaining are ignored (default 100).
    pub max_attachment_count: usize,
    /// Max line length for a single header before truncation (default 8 KB).
    pub max_header_line_length: usize,
    /// Max single header value before truncation (default 64 KB).
    pub max_single_header_value: usize,
    /// HTML parser node budget before conversion is aborted (default 100k nodes).
    pub html_parser_node_budget: usize,
    /// Feed response size before rejection (default 10 MB).
    pub feed_response_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_raw_message_size: MAX_RAW_MESSAGE_SIZE,
            max_mime_nesting_depth: MAX_MIME_NESTING_DEPTH,
            max_parts_per_message: MAX_PARTS_PER_MESSAGE,
            max_extracted_body_size: MAX_EXTRACTED_BODY_SIZE,
            max_attachment_count: MAX_ATTACHMENT_COUNT,
            max_header_line_length: MAX_HEADER_LINE_LENGTH,
            max_single_header_value: MAX_SINGLE_HEADER_VALUE,
            html_parser_node_budget: HTML_PARSER_NODE_BUDGET,
            feed_response_size: FEED_RESPONSE_SIZE_LIMIT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limits_defaults() {
        let limits = Limits::default();
        assert_eq!(limits.max_raw_message_size, MAX_RAW_MESSAGE_SIZE);
        assert_eq!(limits.max_mime_nesting_depth, MAX_MIME_NESTING_DEPTH);
        assert_eq!(limits.max_parts_per_message, MAX_PARTS_PER_MESSAGE);
        assert_eq!(limits.max_extracted_body_size, MAX_EXTRACTED_BODY_SIZE);
        assert_eq!(limits.max_attachment_count, MAX_ATTACHMENT_COUNT);
        assert_eq!(limits.max_header_line_length, MAX_HEADER_LINE_LENGTH);
        assert_eq!(limits.max_single_header_value, MAX_SINGLE_HEADER_VALUE);
        assert_eq!(limits.html_parser_node_budget, HTML_PARSER_NODE_BUDGET);
        assert_eq!(limits.feed_response_size, FEED_RESPONSE_SIZE_LIMIT);
    }
}

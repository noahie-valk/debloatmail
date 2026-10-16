/// Configurable limits for hostile-content parsing per MIME-AND-TEXT.md §13.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Max raw message size before aborting body parse (default 25 MB).
    pub max_raw_message_size: usize,
    /// Max MIME nesting depth before recursion stops (default 20).
    pub max_mime_nesting_depth: usize,
    /// Max total parts per message before remaining are ignored (default 500).
    pub max_parts_per_message: usize,
    /// Max extracted body size in bytes before truncating at paragraph boundary (default 1 MB).
    pub max_extracted_body_size: usize,
    /// Max attachment parts recorded before remaining are ignored (default 100).
    pub max_attachment_count: usize,
    /// Max line length for a single header before truncation (default 8 KB).
    pub max_header_line_length: usize,
    /// Max single header value before truncation (default 64 KB).
    pub max_single_header_value: usize,
    /// HTML parser node budget before conversion is aborted (default 100k nodes).
    pub html_parser_node_budget: usize,
    /// Feed response size before rejection (default 10 MB).
    pub feed_response_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_raw_message_size: 25 * 1024 * 1024, // 25 MB
            max_mime_nesting_depth: 20,
            max_parts_per_message: 500,
            max_extracted_body_size: 1024 * 1024, // 1 MB
            max_attachment_count: 100,
            max_header_line_length: 8 * 1024,   // 8 KB
            max_single_header_value: 64 * 1024, // 64 KB
            html_parser_node_budget: 100_000,
            feed_response_size: 10 * 1024 * 1024, // 10 MB
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limits_defaults() {
        let limits = Limits::default();
        assert_eq!(limits.max_raw_message_size, 25 * 1024 * 1024);
        assert_eq!(limits.max_mime_nesting_depth, 20);
        assert_eq!(limits.max_parts_per_message, 500);
        assert_eq!(limits.max_extracted_body_size, 1024 * 1024);
        assert_eq!(limits.max_attachment_count, 100);
        assert_eq!(limits.max_header_line_length, 8 * 1024);
        assert_eq!(limits.max_single_header_value, 64 * 1024);
        assert_eq!(limits.html_parser_node_budget, 100_000);
        assert_eq!(limits.feed_response_size, 10 * 1024 * 1024);
    }
}

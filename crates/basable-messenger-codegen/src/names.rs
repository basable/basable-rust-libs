//! The naming rules, byte-identical to the scaffolder's template functions
//! in the monorepo (`golang/controller/lib/scaffold/render.go`: `snake`,
//! `pascal`), because the scaffold writes `handle_<snake message>` in a
//! nanoservice's `handlers.rs` and the generator must produce the same
//! method name.

/// `GetProductRequest` → `get_product_request`; `HTTPRequest` →
/// `http_request`; separators `-`, ` ` and `.` become `_`.
pub fn snake(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev_lower = false;
    for (i, &c) in chars.iter().enumerate() {
        if c == '-' || c == ' ' || c == '.' {
            out.push('_');
            prev_lower = false;
        } else if c.is_uppercase() {
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if i > 0 && (prev_lower || next_lower) && !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(c.to_lowercase());
            prev_lower = false;
        } else {
            out.push(c);
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

/// `order_ops` → `OrderOps` (splits on `_`, `-` and ` `).
pub fn pascal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for part in s.split(['_', '-', ' ']) {
        if part.is_empty() {
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// The last segment of a Rust path: `messages::GetProductRequest` →
/// `GetProductRequest`.
pub fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_matches_the_scaffolders_function() {
        // The same inputs and outputs the Go render_test pins.
        assert_eq!(snake("GetProductRequest"), "get_product_request");
        assert_eq!(snake("HTTPRequest"), "http_request");
        assert_eq!(snake("OrderEvent"), "order_event");
        assert_eq!(snake("already_snake"), "already_snake");
        assert_eq!(snake("kebab-case"), "kebab_case");
        assert_eq!(snake("Order2Event"), "order2_event");
        assert_eq!(snake("ABC"), "abc");
        assert_eq!(
            snake("SendNotificationRequest"),
            "send_notification_request"
        );
    }

    #[test]
    fn pascal_matches_the_scaffolders_function() {
        assert_eq!(pascal("order_ops"), "OrderOps");
        assert_eq!(pascal("api"), "Api");
        assert_eq!(pascal("a-b c"), "ABC");
        assert_eq!(pascal("__x"), "X");
    }

    #[test]
    fn last_segment_strips_the_path() {
        assert_eq!(
            last_segment("messages::GetProductRequest"),
            "GetProductRequest"
        );
        assert_eq!(last_segment("GetProductRequest"), "GetProductRequest");
    }
}

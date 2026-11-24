//! Safe, non-executable message templating.
//!
//! Templates contain `{{ path.to.value }}` placeholders, optionally followed
//! by `| filter`. There are no loops, conditionals or expressions, so message
//! content can never be interpreted as template code.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::Value as Json;

/// How substituted values are escaped for the destination format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escape {
    /// Plain text; no escaping.
    None,
    /// Inside a JSON string literal.
    Json,
    /// URL component.
    Url,
    Html,
    /// Markdown as used by Discord and Microsoft Teams, plus mention neutralisation.
    Markdown,
    /// Slack mrkdwn control characters, plus mention neutralisation.
    Slack,
    /// Plain chat text where only mentions need neutralising (Telegram).
    Chat,
}

fn neutralise_mentions(s: &str) -> String {
    // A zero-width space after '@' prevents @everyone, @here and user mentions.
    s.replace('@', "@\u{200b}")
}

pub fn escape(value: &str, mode: Escape) -> String {
    match mode {
        Escape::None => value.to_string(),
        Escape::Json => {
            let quoted = serde_json::to_string(value).unwrap_or_default();
            quoted[1..quoted.len().saturating_sub(1)].to_string()
        }
        Escape::Url => utf8_percent_encode(value, NON_ALPHANUMERIC).to_string(),
        Escape::Html => value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;"),
        Escape::Markdown => {
            let mut out = String::with_capacity(value.len() + 8);
            for c in value.chars() {
                if "\\*_~`|>[]()#<".contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            neutralise_mentions(&out)
        }
        Escape::Slack => neutralise_mentions(
            &value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;"),
        ),
        Escape::Chat => neutralise_mentions(value),
    }
}

/// Resolve a dotted path inside a JSON value.
pub fn lookup<'a>(ctx: &'a Json, path: &str) -> Option<&'a Json> {
    let mut cur = ctx;
    for seg in path.split('.') {
        cur = match cur {
            Json::Object(o) => o.get(seg)?,
            Json::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn display(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Render a template. Unknown paths render as an empty string.
pub fn render(template: &str, ctx: &Json, default: Escape) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let expr = &after[..end];
        let (path, filter) = match expr.split_once('|') {
            Some((p, f)) => (p.trim(), Some(f.trim())),
            None => (expr.trim(), None),
        };
        let mode = match filter {
            Some("raw") => Escape::None,
            Some("json") => Escape::Json,
            Some("url") => Escape::Url,
            Some("html") => Escape::Html,
            Some("md") => Escape::Markdown,
            _ => default,
        };
        let value = lookup(ctx, path).map(display).unwrap_or_default();
        out.push_str(&escape(&value, mode));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> Json {
        json!({"message": {"sender": "+4500000000", "body": "hi <b> \"q\" @everyone *x* {{event.id}}", "parts": 2}, "event": {"id": "E1"}})
    }

    #[test]
    fn substitutes_and_ignores_unknown() {
        assert_eq!(
            render(
                "From {{message.sender}} ({{ message.parts }}) {{nope.x}}!",
                &ctx(),
                Escape::None
            ),
            "From +4500000000 (2) !"
        );
        assert_eq!(
            render("unterminated {{message.sender", &ctx(), Escape::None),
            "unterminated {{message.sender"
        );
    }

    #[test]
    fn values_are_not_reinterpreted_as_templates() {
        let out = render("{{message.body}}", &ctx(), Escape::None);
        assert!(out.contains("{{event.id}}"));
        assert!(!out.contains("E1"));
    }

    #[test]
    fn escaping_per_destination() {
        let json_body = render("{\"text\": \"{{message.body}}\"}", &ctx(), Escape::Json);
        let parsed: Json = serde_json::from_str(&json_body).unwrap();
        assert_eq!(parsed["text"], ctx()["message"]["body"]);
        assert_eq!(
            render("{{message.body}}", &ctx(), Escape::Html),
            "hi &lt;b&gt; &quot;q&quot; @everyone *x* {{event.id}}"
        );
        assert!(render("{{message.sender}}", &ctx(), Escape::Url).starts_with("%2B45"));
        let md = render("{{message.body}}", &ctx(), Escape::Markdown);
        assert!(
            md.contains("\\*x\\*") && md.contains("@\u{200b}everyone") && md.contains("\\<b\\>")
        );
        let slack = render(
            "{{message.body}}",
            &json!({"message": {"body": "<!channel> <@U1> & @here"}}),
            Escape::Slack,
        );
        assert_eq!(
            slack,
            "&lt;!channel&gt; &lt;@\u{200b}U1&gt; &amp; @\u{200b}here"
        );
        assert_eq!(
            render(
                "{{message.sender|url}} {{message.sender|raw}}",
                &ctx(),
                Escape::Html
            ),
            "%2B4500000000 +4500000000"
        );
    }
}

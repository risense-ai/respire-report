//! Markdown只提供结构事件；邮件标签、样式与链接权限由本模块白名单决定。
use super::mail::html_escape;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// 链接仅接受完整HTTP(S)地址；不对本地路径、协议相对地址或控制字符作浏览器式猜测。
fn web_link(value: &str) -> Option<String> {
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    if !(value.to_ascii_lowercase().starts_with("https://")
        || value.to_ascii_lowercase().starts_with("http://"))
    {
        return None;
    }
    let parsed = url::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    Some(parsed.into())
}

/// 纯文本part保留原文；此处仅渲染HTML part，不解释原始HTML、不加载图片或其他远程资源。
pub(super) fn render(input: &str) -> String {
    let mut out = String::new();
    let mut endings: Vec<String> = Vec::new();
    let mut image_depth = 0usize;
    let mut table_head = false;
    for event in Parser::new_ext(
        input,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    ) {
        // 图片仅保留替代文字，嵌套标记也不会生成链接或加载标签。
        if image_depth > 0 {
            match event {
                Event::Start(Tag::Image { .. }) => image_depth += 1,
                Event::End(TagEnd::Image) => image_depth -= 1,
                Event::Text(v) | Event::Code(v) | Event::Html(v) | Event::InlineHtml(v) => {
                    out.push_str(&html_escape(&v))
                }
                Event::SoftBreak | Event::HardBreak => out.push(' '),
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::Image { .. }) => image_depth = 1,
            Event::Start(tag) => {
                let (open, close) = match tag {
                    Tag::Paragraph => ("<p style=\"margin:8px 0\">".into(), "</p>".into()),
                    Tag::Heading { .. } => ("<h3 style=\"font-size:16px;font-weight:700;margin:18px 0 8px;color:#273142\">".into(), "</h3>".into()),
                    Tag::BlockQuote(_) => ("<blockquote style=\"margin:12px 0;padding:8px 14px;border-left:3px solid #cbd5e1;background:#f8fafc;color:#475569\">".into(), "</blockquote>".into()),
                    Tag::CodeBlock(_) => ("<pre style=\"background:#f1f5f9;border:1px solid #e2e8f0;border-radius:4px;padding:12px;white-space:pre-wrap;overflow-wrap:anywhere;font-family:Consolas,monospace;font-size:14px\"><code>".into(), "</code></pre>".into()),
                    Tag::List(Some(start)) => (format!("<ol start=\"{start}\" style=\"padding-left:24px;margin:8px 0\">"), "</ol>".into()),
                    Tag::List(None) => ("<ul style=\"padding-left:24px;margin:8px 0\">".into(), "</ul>".into()),
                    Tag::Item => ("<li style=\"margin:4px 0\">".into(), "</li>".into()),
                    Tag::Emphasis => ("<em>".into(), "</em>".into()),
                    Tag::Strong => ("<strong style=\"font-weight:700\">".into(), "</strong>".into()),
                    Tag::Strikethrough => ("<del>".into(), "</del>".into()),
                    Tag::Table(_) => ("<table style=\"border-collapse:collapse;width:100%;margin:12px 0;font-size:14px\">".into(), "</tbody></table>".into()),
                    Tag::TableHead => { table_head=true; ("<thead><tr>".into(), "</tr></thead><tbody>".into()) },
                    Tag::TableRow => ("<tr>".into(), "</tr>".into()),
                    Tag::TableCell => { let name=if table_head {"th"} else {"td"}; (format!("<{name} style=\"border:1px solid #cbd5e1;padding:8px;text-align:left;vertical-align:top\">"), format!("</{name}>")) },
                    Tag::Link { dest_url, .. } => match web_link(&dest_url) {
                        Some(url) => (format!("<a href=\"{}\" style=\"color:#2563eb;text-decoration:underline\">",html_escape(&url)), "</a>".into()),
                        None => ("<span>".into(), format!(" ({})</span>",html_escape(&dest_url))),
                    },
                    // 未启用的扩展和原始HTML容器不产生任何可执行标签。
                    _ => (String::new(), String::new()),
                };
                out.push_str(&open); endings.push(close);
            }
            Event::End(tag) => {
                if tag==TagEnd::TableHead { table_head=false; }
                if let Some(close)=endings.pop() { out.push_str(&close); }
            }
            Event::Code(v) => out.push_str(&format!("<code style=\"background:#f1f5f9;padding:2px 4px;border-radius:3px;font-family:Consolas,monospace;font-size:14px\">{}</code>",html_escape(&v))),
            Event::Text(v) | Event::Html(v) | Event::InlineHtml(v) | Event::InlineMath(v) | Event::DisplayMath(v) | Event::FootnoteReference(v) => out.push_str(&html_escape(&v)),
            Event::SoftBreak | Event::HardBreak => out.push_str("<br>"),
            Event::Rule => out.push_str("<hr style=\"border:0;border-top:1px solid #e2e8f0;margin:16px 0\">"),
            Event::TaskListMarker(done) => out.push_str(if done {"[x] "} else {"[ ] "}),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markdown_structure_and_untrusted_content() {
        let body = render(
            "# Heading\n\n**bold** and `inline`\n\n- item\n\n> quote\n\n```html\n<script>bad</script>\n```\n\n| A | B |\n|---|---|\n| X | Y |\n\n[web](https://example.invalid/a?x=1&y=2)\n\n![alt](https://example.invalid/pixel)\n\n<script>alert(1)</script>",
        );
        for tag in [
            "<h3 ",
            "<strong ",
            "<code ",
            "<ul ",
            "<blockquote ",
            "<pre ",
            "<table ",
            "<th ",
            "<td ",
        ] {
            assert!(body.contains(tag), "{tag}");
        }
        assert!(body.contains("href=\"https://example.invalid/a?x=1&amp;y=2\""));
        assert!(
            body.contains("&lt;script&gt;")
                && !body.contains("<script>")
                && !body.contains("<img")
                && !body.contains("src=")
        );
        for url in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "data:text/html,bad",
            "file:///C:/secret",
            "C:/project/a.rs",
            "/D:/path/a.rs",
            "//example.invalid",
            "java\nscript:bad",
            "https%3A%2F%2Fexample.invalid",
        ] {
            assert!(web_link(url).is_none(), "{url}");
        }
        let unsafe_links =
            render("[a](javascript&#58;alert) [b](data:text/html,bad) [c](/D:/project/a.rs)");
        assert!(!unsafe_links.contains("href="));
        assert!(unsafe_links.contains("/D:/project/a.rs"));
    }
}

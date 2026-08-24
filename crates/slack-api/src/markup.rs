//! A parser for Slack's `mrkdwn`.
//!
//! Slack does not send HTML or CommonMark. It sends its own dialect where
//! links, mentions, and channel references are angle-bracket escapes
//! (`<@U123|alice>`), emphasis is single-character (`*bold*`, `_italic_`), and
//! the five XML entities are escaped. This module turns that into blocks and
//! spans the view layer can render directly, so no renderer has to re-derive
//! Slack's escaping rules.

/// A run of text sharing one appearance and destination.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<Link>,
}

impl Span {
    fn plain(text: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            ..Default::default()
        }
    }
}

/// Where a span points, when it points anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// An ordinary URL, opened by the browser.
    Url(String),
    /// `<@U123>` — a workspace member.
    User(String),
    /// `<#C123|general>` — another conversation.
    Channel(String),
    /// `<!here>`, `<!channel>`, `<!subteam^S1|@design>`.
    Broadcast(String),
}

/// One rendered line-level unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Quote(Vec<Span>),
    /// A fenced block; the text is kept verbatim, including newlines.
    Code(String),
    /// A `•`/`-`/`1.` item. `depth` counts leading indent levels.
    ListItem {
        spans: Vec<Span>,
        depth: usize,
        ordered: bool,
    },
}

/// Parse a Slack message body into renderable blocks.
pub fn parse(input: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut rest = input;

    // Fenced code is scanned first: nothing inside a fence is markup.
    while let Some(start) = rest.find("```") {
        let (before, after_fence) = rest.split_at(start);
        push_line_blocks(before, &mut blocks);

        let body = &after_fence[3..];
        match body.find("```") {
            Some(end) => {
                blocks.push(Block::Code(body[..end].trim_matches('\n').to_string()));
                rest = &body[end + 3..];
            }
            None => {
                // An unterminated fence runs to the end of the message, which
                // is what Slack itself renders.
                blocks.push(Block::Code(body.trim_matches('\n').to_string()));
                rest = "";
            }
        }
    }
    push_line_blocks(rest, &mut blocks);

    if blocks.is_empty() {
        blocks.push(Block::Paragraph(Vec::new()));
    }
    blocks
}

/// Collapse a message to plain text — used for previews, notifications, and
/// the sidebar's last-message line.
pub fn to_plain_text(input: &str) -> String {
    let mut out = String::new();
    for block in parse(input) {
        if !out.is_empty() {
            out.push(' ');
        }
        match block {
            Block::Code(code) => out.push_str(&code.replace('\n', " ")),
            Block::Paragraph(spans) | Block::Quote(spans) => {
                out.extend(spans.iter().map(|s| s.text.as_str()))
            }
            Block::ListItem { spans, .. } => out.extend(spans.iter().map(|s| s.text.as_str())),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_line_blocks(input: &str, blocks: &mut Vec<Block>) {
    if input.is_empty() {
        return;
    }

    let mut paragraph: Vec<Span> = Vec::new();

    for (ix, line) in input.split('\n').enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if let Some(quoted) = trimmed
            .strip_prefix("&gt;")
            .or_else(|| trimmed.strip_prefix('>'))
        {
            flush(&mut paragraph, blocks);
            blocks.push(Block::Quote(parse_spans(quoted.trim_start())));
            continue;
        }

        if let Some(item) = trimmed
            .strip_prefix("• ")
            .or_else(|| trimmed.strip_prefix("- "))
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush(&mut paragraph, blocks);
            blocks.push(Block::ListItem {
                spans: parse_spans(item),
                depth: indent / 4,
                ordered: false,
            });
            continue;
        }

        if let Some(item) = strip_ordered_marker(trimmed) {
            flush(&mut paragraph, blocks);
            blocks.push(Block::ListItem {
                spans: parse_spans(item),
                depth: indent / 4,
                ordered: true,
            });
            continue;
        }

        // Blank line ends the paragraph; otherwise the line joins it.
        if trimmed.is_empty() {
            flush(&mut paragraph, blocks);
            continue;
        }
        if ix > 0 && !paragraph.is_empty() {
            paragraph.push(Span::plain("\n"));
        }
        paragraph.extend(parse_spans(line));
    }

    flush(&mut paragraph, blocks);
}

fn flush(paragraph: &mut Vec<Span>, blocks: &mut Vec<Block>) {
    if !paragraph.is_empty() {
        blocks.push(Block::Paragraph(std::mem::take(paragraph)));
    }
}

fn strip_ordered_marker(line: &str) -> Option<&str> {
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    line[digits.len()..]
        .strip_prefix(". ")
        .or_else(|| line[digits.len()..].strip_prefix(") "))
}

/// Parse the inline markup of a single line.
fn parse_spans(line: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut style = Style::default();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // `<…>` escapes: links, mentions, channels, broadcasts.
        if c == '<'
            && let Some(close) = find_char(&chars, i + 1, '>')
        {
            let body: String = chars[i + 1..close].iter().collect();
            if let Some(span) = parse_escape(&body, style) {
                push_buffer(&mut buffer, style, &mut spans);
                spans.push(span);
                i = close + 1;
                continue;
            }
        }

        // Inline code wins over every other emphasis marker.
        if c == '`'
            && let Some(close) = find_char(&chars, i + 1, '`')
            && close > i + 1
        {
            push_buffer(&mut buffer, style, &mut spans);
            let mut code = style;
            code.code = true;
            spans.push(span_from(chars[i + 1..close].iter().collect(), code));
            i = close + 1;
            continue;
        }

        if let Some(marker) = emphasis_marker(c)
            && can_toggle(&chars, i, c, style.has(marker))
        {
            push_buffer(&mut buffer, style, &mut spans);
            style.toggle(marker);
            i += 1;
            continue;
        }

        buffer.push(c);
        i += 1;
    }

    push_buffer(&mut buffer, style, &mut spans);
    spans
}

fn find_char(chars: &[char], from: usize, needle: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|c| *c == needle)
        .map(|p| p + from)
}

/// Decode one `<…>` escape. Returns `None` when the body is not one Slack
/// produces, so the caller can fall back to literal text.
fn parse_escape(body: &str, style: Style) -> Option<Span> {
    let (target, label) = match body.split_once('|') {
        Some((target, label)) => (target, Some(label.to_string())),
        None => (body, None),
    };

    let mut span = match target.chars().next()? {
        '@' => {
            let id = &target[1..];
            if !is_slack_id(id) {
                return None;
            }
            Span {
                text: label.unwrap_or_else(|| format!("@{id}")),
                link: Some(Link::User(id.to_string())),
                ..Default::default()
            }
        }
        '#' => {
            let id = &target[1..];
            if !is_slack_id(id) {
                return None;
            }
            Span {
                text: format!("#{}", label.unwrap_or_else(|| id.to_string())),
                link: Some(Link::Channel(id.to_string())),
                ..Default::default()
            }
        }
        '!' => {
            let name = &target[1..];
            // `<!subteam^S123|@design>` names a user group.
            let key = name.split('^').next().unwrap_or(name).to_string();
            Span {
                text: label.unwrap_or_else(|| format!("@{key}")),
                link: Some(Link::Broadcast(key)),
                ..Default::default()
            }
        }
        _ => {
            // Slack wraps every kind of URL this way, not just web ones:
            // `tel:`, `mailto:`, and application schemes all appear. Anything
            // with a scheme is a link; anything else is literal text that
            // merely happened to sit between angle brackets.
            if !has_scheme(target) {
                return None;
            }
            Span {
                text: label.unwrap_or_else(|| target.to_string()),
                link: Some(Link::Url(target.to_string())),
                ..Default::default()
            }
        }
    };

    span.text = unescape_entities(&span.text);
    span.bold = style.bold;
    span.italic = style.italic;
    span.strike = style.strike;
    Some(span)
}

/// Whether `target` looks like `scheme:rest`, per RFC 3986's scheme rules.
fn has_scheme(target: &str) -> bool {
    let Some((scheme, rest)) = target.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn is_slack_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('U' | 'W' | 'C' | 'D' | 'G' | 'S'))
        && id.len() >= 6
        && id.len() <= 32
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// An emphasis marker only toggles when it sits against the text it wraps —
/// `a * b` is arithmetic, `*bold*` is emphasis. Slack applies the same rule,
/// which is why `snake_case_names` survive unstyled.
fn can_toggle(chars: &[char], i: usize, marker: char, closing: bool) -> bool {
    let before = i.checked_sub(1).map(|p| chars[p]);
    let after = chars.get(i + 1).copied();

    if closing {
        let prev_is_text = before.is_some_and(|c| !c.is_whitespace() && c != marker);
        let next_ends_run = after.is_none_or(|c| !c.is_alphanumeric());
        prev_is_text && next_ends_run
    } else {
        let prev_starts_run = before.is_none_or(|c| !c.is_alphanumeric());
        let next_is_text = after.is_some_and(|c| !c.is_whitespace() && c != marker);
        // An unmatched opener would style the rest of the line; require a pair.
        prev_starts_run && next_is_text && chars[i + 1..].contains(&marker)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Bold,
    Italic,
    Strike,
}

impl Style {
    fn has(self, marker: Marker) -> bool {
        match marker {
            Marker::Bold => self.bold,
            Marker::Italic => self.italic,
            Marker::Strike => self.strike,
        }
    }

    fn toggle(&mut self, marker: Marker) {
        match marker {
            Marker::Bold => self.bold = !self.bold,
            Marker::Italic => self.italic = !self.italic,
            Marker::Strike => self.strike = !self.strike,
        }
    }
}

fn emphasis_marker(c: char) -> Option<Marker> {
    match c {
        '*' => Some(Marker::Bold),
        '_' => Some(Marker::Italic),
        '~' => Some(Marker::Strike),
        _ => None,
    }
}

fn push_buffer(buffer: &mut String, style: Style, spans: &mut Vec<Span>) {
    if buffer.is_empty() {
        return;
    }
    let text = unescape_entities(buffer);
    buffer.clear();
    spans.push(span_from(text, style));
}

fn span_from(text: String, style: Style) -> Span {
    Span {
        text,
        bold: style.bold,
        italic: style.italic,
        strike: style.strike,
        code: style.code,
        link: None,
    }
}

/// Slack escapes exactly three characters on the way out; reverse that.
fn unescape_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(input: &str) -> Vec<Span> {
        match parse(input).remove(0) {
            Block::Paragraph(spans) => spans,
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    #[test]
    fn emphasis_applies_only_to_paired_markers() {
        let parsed = spans("*bold* and _italic_ and ~gone~");
        assert!(parsed.iter().any(|s| s.text == "bold" && s.bold));
        assert!(parsed.iter().any(|s| s.text == "italic" && s.italic));
        assert!(parsed.iter().any(|s| s.text == "gone" && s.strike));
    }

    #[test]
    fn underscores_inside_identifiers_are_literal() {
        let parsed = spans("call some_function_name now");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "call some_function_name now");
        assert!(!parsed[0].italic);
    }

    #[test]
    fn an_unmatched_marker_stays_literal() {
        let parsed = spans("2 * 3 = 6");
        assert_eq!(parsed[0].text, "2 * 3 = 6");
    }

    #[test]
    fn inline_code_suppresses_other_markup() {
        let parsed = spans("run `git *push*` today");
        let code = parsed.iter().find(|s| s.code).expect("a code span");
        assert_eq!(code.text, "git *push*");
    }

    #[test]
    fn mentions_carry_their_id_and_label() {
        let parsed = spans("hey <@U0123456|alice> see <#C0123456|general>");
        assert!(
            parsed
                .iter()
                .any(|s| s.text == "alice" && s.link == Some(Link::User("U0123456".into())))
        );
        assert!(
            parsed
                .iter()
                .any(|s| s.text == "#general" && s.link == Some(Link::Channel("C0123456".into())))
        );
    }

    #[test]
    fn a_bare_mention_falls_back_to_the_id() {
        let parsed = spans("<@U0123456> ping");
        assert_eq!(parsed[0].text, "@U0123456");
    }

    #[test]
    fn links_use_their_label_when_slack_supplies_one() {
        let parsed = spans("see <https://example.com|the docs>");
        let link = parsed.last().unwrap();
        assert_eq!(link.text, "the docs");
        assert_eq!(link.link, Some(Link::Url("https://example.com".into())));
    }

    #[test]
    fn links_with_other_schemes_are_recognised() {
        let parsed = spans("call <tel:+16176754444|+1 617-675-4444> now");
        let link = parsed.iter().find(|s| s.link.is_some()).expect("a link");
        assert_eq!(link.text, "+1 617-675-4444");
        assert_eq!(link.link, Some(Link::Url("tel:+16176754444".into())));
    }

    #[test]
    fn angle_brackets_around_ordinary_text_stay_literal() {
        let parsed = spans("a &lt;not a link&gt; b");
        assert_eq!(parsed[0].text, "a <not a link> b");
        assert!(parsed.iter().all(|s| s.link.is_none()));
    }

    #[test]
    fn something_that_is_not_a_scheme_is_not_a_link() {
        assert!(!has_scheme("not a scheme"));
        assert!(!has_scheme("9lives:x"));
        assert!(!has_scheme("http:"));
        assert!(has_scheme("tel:+1"));
        assert!(has_scheme("x-app.v2:open"));
    }

    #[test]
    fn broadcasts_are_recognised() {
        let parsed = spans("<!here> deploy is out");
        assert_eq!(parsed[0].link, Some(Link::Broadcast("here".into())));
    }

    #[test]
    fn entities_are_decoded() {
        let parsed = spans("a &lt;b&gt; &amp; c");
        assert_eq!(parsed[0].text, "a <b> & c");
    }

    #[test]
    fn fenced_code_is_kept_verbatim() {
        let blocks = parse("before\n```\nlet x = *1*;\n```\nafter");
        assert!(matches!(&blocks[1], Block::Code(c) if c == "let x = *1*;"));
    }

    #[test]
    fn an_unterminated_fence_runs_to_the_end() {
        let blocks = parse("oops\n```\nstill code");
        assert!(matches!(blocks.last(), Some(Block::Code(c)) if c == "still code"));
    }

    #[test]
    fn quotes_and_lists_become_their_own_blocks() {
        let blocks = parse("&gt; quoted\n- one\n2. two");
        assert!(matches!(&blocks[0], Block::Quote(_)));
        assert!(matches!(&blocks[1], Block::ListItem { ordered: false, .. }));
        assert!(matches!(&blocks[2], Block::ListItem { ordered: true, .. }));
    }

    #[test]
    fn plain_text_flattens_every_block_kind() {
        let text = to_plain_text("*hi* <@U0123456|bob>\n```code```\n- item");
        assert_eq!(text, "hi bob code item");
    }

    #[test]
    fn an_empty_message_still_yields_one_block() {
        assert_eq!(parse(""), vec![Block::Paragraph(Vec::new())]);
    }
}

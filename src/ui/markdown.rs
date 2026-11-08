//! Markdown for the output panel.
//!
//! Slint's `StyledText` understands a subset of CommonMark (paragraphs, lists, emphasis, inline
//! code, links) and rejects the rest (headings, code blocks, quotes, tables, HTML). Answers use all
//! of it, so [`to_styled`] parses the full syntax and writes it back in that subset: headings become
//! bold lines, code blocks inline-code lines, quotes `│ ` lines, tables `a · b` rows, and HTML plain
//! text. If Slint still refuses the result, the text is shown plain.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use slint::StyledText;

/// Slint's interpolation placeholder (a private-use character); never valid in shown text.
const PLACEHOLDER: char = '\u{e541}';

/// `text` (markdown) as styled text for the panel.
pub fn to_styled(text: &str) -> StyledText {
    StyledText::from_markdown(&to_slint_markdown(text)).unwrap_or_else(|e| {
        log::debug!("markdown shown as plain text: {e}");
        StyledText::from_plain_text(text)
    })
}

/// Rewrite CommonMark in the subset Slint's `StyledText` accepts.
pub fn to_slint_markdown(src: &str) -> String {
    let src = src.replace(PLACEHOLDER, "");
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut w = Writer::default();
    for event in Parser::new_ext(&src, opts) {
        w.event(event);
    }
    w.out.trim_end().to_string()
}

#[derive(Default)]
struct Writer {
    out: String,
    /// One entry per open list: the next number of an ordered list, `None` for bullets.
    lists: Vec<Option<u64>>,
    quote_depth: usize,
    /// Inside a code block: its lines so far.
    code: Option<String>,
    /// Inside a table: the cells of the current row, and whether it is the header.
    row: Option<Vec<String>>,
    cell: Option<String>,
    in_head: bool,
    /// Destinations of the open links, innermost last.
    links: Vec<String>,
    /// Where each open emphasis, strong, strikethrough or heading marker was written, so an
    /// empty one can be taken back (`****` would be a horizontal rule).
    marks: Vec<usize>,
    /// A top-level block ended: the next one starts after an empty line. Slint draws paragraphs
    /// without spacing, so the gap is a line holding a no-break space.
    gap: bool,
}

/// The line that separates top-level blocks.
const GAP: &str = "\u{a0}\n\n";

impl Writer {
    /// Where text goes: the open table cell, else the output.
    fn sink(&mut self) -> &mut String {
        self.cell.as_mut().unwrap_or(&mut self.out)
    }

    fn push(&mut self, s: &str) {
        self.sink().push_str(s);
    }

    /// Open a style marker such as `**`.
    fn open_mark(&mut self, m: &str) {
        let at = self.sink().len();
        self.marks.push(at);
        self.push(m);
    }

    /// Close it, or remove it again if nothing came in between.
    fn close_mark(&mut self, m: &str) {
        let at = self.marks.pop().unwrap_or(0);
        let sink = self.sink();
        if sink.len() == at + m.len() && sink.ends_with(m) {
            sink.truncate(at);
        } else {
            sink.push_str(m);
        }
    }

    /// End the current block: a blank line, unless inside a list (where items follow directly).
    fn end_block(&mut self) {
        let out = &mut self.out;
        while out.ends_with(' ') {
            out.pop();
        }
        if out.is_empty() {
            return;
        }
        if self.lists.is_empty() {
            // Only a block that ends here asks for a gap; an already closed one keeps its state.
            if !out.ends_with("\n\n") {
                out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
                self.gap = self.quote_depth == 0;
            }
        } else if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    /// Before a top-level block: the gap after the previous one, if it asked for one.
    fn begin_block(&mut self) {
        self.end_block();
        if std::mem::take(&mut self.gap) && self.lists.is_empty() && self.quote_depth == 0 && !self.out.is_empty() {
            self.out.push_str(GAP);
        }
    }

    /// Start a new line inside the current block (a soft break), keeping list and quote prefixes.
    fn new_line(&mut self) {
        if self.cell.is_some() {
            self.push(" ");
            return;
        }
        self.out.push('\n');
        self.line_prefix();
    }

    /// Indentation for nested list content, then the quote bar.
    fn line_prefix(&mut self) {
        let indent: usize = self.lists.iter().map(|l| if l.is_some() { 3 } else { 2 }).sum();
        self.out.extend(std::iter::repeat_n(' ', indent));
        for _ in 0..self.quote_depth {
            self.out.push_str("│\u{a0}");
        }
    }

    fn event(&mut self, event: Event<'_>) {
        if let Some(code) = &mut self.code {
            match event {
                Event::Text(t) => code.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    let code = self.code.take().unwrap_or_default();
                    self.code_block(&code);
                }
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                let t = escape(&t);
                self.push(&t);
            }
            Event::Code(t) | Event::InlineMath(t) | Event::DisplayMath(t) => {
                let t = code_span(&t);
                self.push(&t);
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                let t = escape(t.trim_end_matches('\n'));
                self.push(&t);
            }
            Event::FootnoteReference(t) => {
                let t = escape(&format!("[{t}]"));
                self.push(&t);
            }
            Event::SoftBreak | Event::HardBreak => self.new_line(),
            Event::Rule => {
                self.begin_block();
                self.line_prefix();
                self.out.push_str("———");
                self.end_block();
            }
            Event::TaskListMarker(done) => self.push(if done { "☑ " } else { "☐ " }),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.lists.is_empty() {
                    self.begin_block();
                    self.line_prefix_if_new_line();
                } else if self.out.ends_with('\n') {
                    // A later paragraph of a list item: continue the item, indented.
                    self.line_prefix();
                }
                // Otherwise the item's own line (`- `) holds its first paragraph.
            }
            Tag::Heading { level, .. } => {
                self.begin_block();
                self.line_prefix();
                self.open_mark(heading_mark(level));
            }
            Tag::BlockQuote(_) => {
                self.begin_block();
                self.quote_depth += 1;
            }
            // The language tag of a fenced block is dropped; the code speaks for itself.
            Tag::CodeBlock(_) => {
                self.begin_block();
                self.code = Some(String::new());
            }
            Tag::List(first) => {
                if self.lists.is_empty() {
                    self.begin_block();
                } else if !self.out.ends_with('\n') {
                    self.out.push('\n');
                }
                self.lists.push(first);
            }
            Tag::Item => {
                if !self.out.ends_with('\n') && !self.out.is_empty() {
                    self.out.push('\n');
                }
                let depth = self.lists.len().saturating_sub(1);
                let indent: usize = self.lists[..depth].iter().map(|l| if l.is_some() { 3 } else { 2 }).sum();
                self.out.extend(std::iter::repeat_n(' ', indent));
                match self.lists.last_mut() {
                    Some(Some(n)) => {
                        self.out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => self.out.push_str("- "),
                }
            }
            Tag::Emphasis => self.open_mark("*"),
            Tag::Strong => self.open_mark("**"),
            Tag::Strikethrough => self.open_mark("~~"),
            Tag::Link { dest_url, .. } => {
                self.links.push(dest_url.to_string());
                self.push("[");
            }
            Tag::Image { dest_url, .. } => {
                // The alt text as a link to the image.
                self.links.push(dest_url.to_string());
                self.push("[");
            }
            Tag::Table(_) => self.begin_block(),
            Tag::TableHead => {
                self.in_head = true;
                self.row = Some(vec![]);
            }
            Tag::TableRow => self.row = Some(vec![]),
            Tag::TableCell => self.cell = Some(String::new()),
            Tag::FootnoteDefinition(name) => {
                self.end_block();
                let t = escape(&format!("[{name}] "));
                self.out.push_str(&t);
            }
            Tag::HtmlBlock
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_)
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    fn line_prefix_if_new_line(&mut self) {
        if self.out.is_empty() || self.out.ends_with('\n') {
            self.line_prefix();
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Table => self.end_block(),
            TagEnd::Heading(level) => {
                self.close_mark(heading_mark(level));
                self.end_block();
                // A heading belongs to what follows it.
                self.gap = false;
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.end_block();
                // The quote's own paragraphs had no gap; the block after the quote does.
                self.gap |= self.quote_depth == 0 && self.lists.is_empty();
            }
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.end_block();
                }
            }
            TagEnd::Item => {
                if !self.out.ends_with('\n') {
                    self.out.push('\n');
                }
            }
            TagEnd::Emphasis => self.close_mark("*"),
            TagEnd::Strong => self.close_mark("**"),
            TagEnd::Strikethrough => self.close_mark("~~"),
            TagEnd::Link | TagEnd::Image => {
                let url = self.links.pop().unwrap_or_default();
                let t = format!("]({})", link_dest(&url));
                self.push(&t);
            }
            TagEnd::TableCell => {
                if let (Some(cell), Some(row)) = (self.cell.take(), self.row.as_mut()) {
                    row.push(cell.trim().to_string());
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                let cells = self.row.take().unwrap_or_default();
                self.line_prefix();
                let line = cells.join(" · ");
                if self.in_head && !line.is_empty() {
                    self.out.push_str(&format!("**{line}**"));
                } else {
                    self.out.push_str(&line);
                }
                self.out.push('\n');
                self.in_head = false;
            }
            TagEnd::CodeBlock
            | TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_)
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    /// Each line as inline code on a line of its own; blank lines stay (as a no-break space).
    fn code_block(&mut self, code: &str) {
        let lines: Vec<&str> = code.trim_end_matches('\n').split('\n').collect();
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                self.out.push('\n');
            }
            self.line_prefix();
            if line.trim().is_empty() {
                self.out.push('\u{a0}');
            } else {
                // Leading spaces would be trimmed; no-break spaces keep the indentation.
                let indent = line.len() - line.trim_start_matches(' ').len();
                let line = format!("{}{}", "\u{a0}".repeat(indent), &line[indent..]);
                self.out.push_str(&code_span(&line.replace('\t', "\u{a0}\u{a0}\u{a0}\u{a0}")));
            }
        }
        self.end_block();
    }
}

/// Big headings are bold, small ones italic.
fn heading_mark(level: HeadingLevel) -> &'static str {
    if level <= HeadingLevel::H2 { "**" } else { "*" }
}

/// Backslash-escape everything Markdown could read as syntax.
fn escape(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Inline code with a fence longer than any backtick run inside.
fn code_span(t: &str) -> String {
    let t = t.replace('\n', " ");
    let longest = t.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if t.starts_with('`') || t.ends_with('`') || t.starts_with(' ') && t.ends_with(' ') { " " } else { "" };
    format!("{fence}{pad}{t}{pad}{fence}")
}

/// A link destination that cannot end the link early.
fn link_dest(url: &str) -> String {
    let url = url.replace(' ', "%20").replace('(', "%28").replace(')', "%29").replace('<', "%3C").replace('>', "%3E");
    if url.is_empty() { "#".into() } else { url }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slint accepts the rewrite of every input below.
    fn accepted(md: &str) -> String {
        let out = to_slint_markdown(md);
        if let Err(e) = StyledText::from_markdown(&out) {
            panic!("rejected: {e}\n--- input:\n{md}\n--- rewritten:\n{out}");
        }
        out
    }

    #[test]
    fn plain_text_and_inline_styles_pass_through() {
        assert_eq!(accepted("Hello **bold** and *it* and `code`."), "Hello **bold** and *it* and `code`\\.");
        assert_eq!(accepted("a\nb"), "a\nb");
        assert_eq!(accepted("one\n\ntwo"), "one\n\n\u{a0}\n\ntwo", "a gap line between blocks");
        assert_eq!(accepted("see [docs](<https://x.dev/a b>)"), "see [docs](https://x.dev/a%20b)");
        assert_eq!(accepted("#"), "");
        assert_eq!(accepted("****"), "———", "a rule stays a rule");
    }

    #[test]
    fn unsupported_blocks_are_rewritten() {
        assert_eq!(accepted("# Title\n\ntext"), "**Title**\n\ntext", "no gap after a heading");
        assert_eq!(accepted("### Small"), "*Small*");
        assert_eq!(accepted("```rust\nfn main() {\n    x\n}\n```"), "`fn main() {`\n`\u{a0}\u{a0}\u{a0}\u{a0}x`\n`}`");
        assert_eq!(accepted("> quoted\n> more"), "│\u{a0}quoted\n│\u{a0}more");
        assert_eq!(accepted("> q\n\nafter"), "│\u{a0}q\n\n\u{a0}\n\nafter");
        assert_eq!(accepted("| a | b |\n|---|---|\n| 1 | 2 |"), "**a · b**\n1 · 2");
        assert_eq!(accepted("x\n\n---\n\ny"), "x\n\n\u{a0}\n\n———\n\n\u{a0}\n\ny");
        assert_eq!(accepted("- [x] done\n- [ ] open"), "- ☑ done\n- ☐ open");
    }

    #[test]
    fn lists_keep_numbers_and_nesting() {
        assert_eq!(accepted("1. one\n2. two\n   - sub\n3. three"), "1. one\n2. two\n   - sub\n3. three");
        assert_eq!(accepted("- a\n\n- b"), "- a\n- b");
        assert_eq!(accepted("3. three\n4. four"), "3. three\n4. four");
    }

    #[test]
    fn markup_characters_in_text_stay_text() {
        // HTML, generics, braces, the placeholder: shown as typed.
        accepted("Use `Vec<String>` or Vec<String> and <b>html</b> and {} and {0} \u{e541} here");
        accepted("<div>\nblock html\n</div>");
        assert_eq!(accepted("a_b_c * not emphasis"), "a\\_b\\_c \\* not emphasis");
        assert_eq!(accepted("``code with ` tick``"), "``code with ` tick``");
    }

    #[test]
    fn messy_and_partial_input_never_fails() {
        // Streaming cuts text anywhere; every prefix must still be accepted.
        let answer = "# Plan\n\n1. **Read** the [guide](https://e.x/g).\n2. Run:\n\n   ```sh\n   cargo test\n   ```\n\n> Note: *careful* with `unsafe`\n\n| k | v |\n|---|---|\n| a | `b` |\n\n- [ ] item <br> html\n\n---\nEnd ~~old~~ new.";
        for (i, _) in answer.char_indices() {
            accepted(&answer[..i]);
        }
        accepted(answer);
        for odd in ["**", "*", "`", "[", "](", "<", ">", "\\", "***x", "[a](b", "- ", "1.", "#", "| |", "~~"] {
            accepted(odd);
        }
    }

    #[test]
    fn random_markup_is_always_accepted() {
        const CHARS: &[char] = &[
            '*', '_', '`', '[', ']', '(', ')', '<', '>', '#', '-', '+', '|', '~', '\n', '\n', ' ', ' ', '!', '1', '.',
            ':', '\\', '{', '}', 'a', 'b', 'x', '\t', '=', '&', ';',
        ];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let len = (next() % 48) as usize;
            let text: String = (0..len).map(|_| CHARS[(next() % CHARS.len() as u64) as usize]).collect();
            accepted(&text);
        }
    }

    #[test]
    fn styled_text_falls_back_to_plain() {
        // Whatever happens, something is shown.
        let s = to_styled("**hi** there");
        assert_ne!(s, StyledText::default());
    }
}

use std::mem;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub const MARKDOWN_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NoteDraft {
    pub title: String,
    pub markdown: String,
}

impl NoteDraft {
    pub fn validated(mut self) -> Result<Self, &'static str> {
        self.title = self.title.trim().to_owned();
        if self.title.is_empty() || self.title.len() > 256 || self.title.contains(char::is_control) {
            return Err("请输入有效标题（最多 256 字节）");
        }
        if self.markdown.len() > MARKDOWN_LIMIT || self.markdown.contains('\0') {
            return Err("笔记正文最多 64 KiB，且不能包含空字符");
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoteVersion {
    pub id: i64,
    pub revision: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Note {
    pub version: NoteVersion,
    pub draft: NoteDraft,
}

pub fn note_entry_id(id: i64) -> String {
    format!("note:{id}")
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MarkdownBlockKind {
    #[default]
    Paragraph,
    Heading(u8),
    Code,
    Rule,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownStyle {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub link: bool,
    pub code: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSpan {
    pub len: usize,
    pub style: MarkdownStyle,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarkdownBlock {
    pub kind: MarkdownBlockKind,
    pub list_depth: usize,
    pub quoted: bool,
    pub text: String,
    pub spans: Vec<MarkdownSpan>,
}

pub fn markdown_preview(markdown: &str) -> Vec<MarkdownBlock> {
    let options = Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut builder = PreviewBuilder::default();
    for event in Parser::new_ext(markdown, options) {
        builder.event(event);
    }
    builder.flush();
    builder.blocks
}

#[derive(Default)]
struct PreviewBuilder {
    blocks: Vec<MarkdownBlock>,
    block: MarkdownBlock,
    lists: Vec<Option<u64>>,
    quotes: usize,
    strong: usize,
    emphasis: usize,
    strike: usize,
    links: usize,
}

impl PreviewBuilder {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                self.flush();
                self.block.kind = MarkdownBlockKind::Heading(level as u8);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.flush();
                self.quotes += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.flush();
                self.quotes = self.quotes.saturating_sub(1);
            }
            Event::Start(Tag::CodeBlock(_)) => {
                self.flush();
                self.block.kind = MarkdownBlockKind::Code;
            }
            Event::Start(Tag::List(start)) => {
                self.flush();
                self.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                self.flush();
                self.lists.pop();
            }
            Event::Start(Tag::Item) => {
                self.flush();
                if let Some(Some(number)) = self.lists.last_mut() {
                    let label = format!("{number}. ");
                    *number = number.saturating_add(1);
                    self.append(&label, false);
                } else {
                    self.append("• ", false);
                }
            }
            Event::End(TagEnd::Heading(_) | TagEnd::Paragraph | TagEnd::Item | TagEnd::CodeBlock | TagEnd::HtmlBlock) => self.flush(),
            Event::Start(Tag::Strong) => self.strong += 1,
            Event::End(TagEnd::Strong) => self.strong = self.strong.saturating_sub(1),
            Event::Start(Tag::Emphasis) => self.emphasis += 1,
            Event::End(TagEnd::Emphasis) => self.emphasis = self.emphasis.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => self.strike += 1,
            Event::End(TagEnd::Strikethrough) => self.strike = self.strike.saturating_sub(1),
            Event::Start(Tag::Link { .. }) => self.links += 1,
            Event::End(TagEnd::Link) => self.links = self.links.saturating_sub(1),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => self.append(&text, false),
            Event::Code(text) => self.append(&text, true),
            Event::SoftBreak => self.append(" ", false),
            Event::HardBreak => self.append("\n", false),
            Event::Rule => {
                self.flush();
                self.block.kind = MarkdownBlockKind::Rule;
                self.block.quoted = self.quotes > 0;
                self.block.list_depth = self.lists.len();
                self.flush();
            }
            Event::TaskListMarker(true) => self.append("☑ ", false),
            Event::TaskListMarker(false) => self.append("☐ ", false),
            _ => {}
        }
    }

    fn append(&mut self, text: &str, code: bool) {
        if text.is_empty() {
            return;
        }
        if self.block.text.is_empty() {
            self.block.quoted = self.quotes > 0;
            self.block.list_depth = self.lists.len();
        }
        let style = MarkdownStyle {
            strong: self.strong > 0,
            emphasis: self.emphasis > 0,
            strike: self.strike > 0,
            link: self.links > 0,
            code: code || self.block.kind == MarkdownBlockKind::Code,
        };
        self.block.text.push_str(text);
        if let Some(last) = self.block.spans.last_mut() && last.style == style {
            last.len += text.len();
        } else {
            self.block.spans.push(MarkdownSpan { len: text.len(), style });
        }
    }

    fn flush(&mut self) {
        let block = mem::take(&mut self.block);
        if !block.text.is_empty() || block.kind == MarkdownBlockKind::Rule {
            self.blocks.push(block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_sections_and_preserves_utf8_run_boundaries() {
        let blocks = markdown_preview("# 标题\n\n> **加粗**与`代码`\n\n3. 已有条目\n\n```rust\nlet x = 1;\n```\n\n---");
        assert_eq!(blocks[0].kind, MarkdownBlockKind::Heading(1));
        assert_eq!(blocks[0].text, "标题");
        assert!(blocks[1].quoted);
        assert!(blocks[1].spans.iter().any(|span| span.style.strong));
        assert!(blocks[1].spans.iter().any(|span| span.style.code));
        assert_eq!(blocks[2].text, "3. 已有条目");
        assert_eq!(blocks[3].kind, MarkdownBlockKind::Code);
        assert_eq!(blocks[4].kind, MarkdownBlockKind::Rule);
        for block in blocks {
            let mut end = 0;
            for span in block.spans {
                end += span.len;
                assert!(block.text.is_char_boundary(end));
            }
            assert_eq!(end, block.text.len());
        }
    }
}

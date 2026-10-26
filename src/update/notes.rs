//! Render Release Markdown as native blocks, using the existing CommonMark parser.
//! Remote notes are display data; HTML and remote images are never executed.
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Body,
    Heading,
    Code,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub text: String,
    pub style: Style,
}

pub fn blocks(markdown: &str) -> Vec<Block> {
    let mut result = Vec::new();
    let mut text = String::new();
    let mut style = Style::Body;
    fn flush(result: &mut Vec<Block>, text: &mut String, style: Style) {
        if !text.trim().is_empty() {
            result.push(Block {
                text: text.trim_end().into(),
                style,
            });
        }
        text.clear();
    }
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                flush(&mut result, &mut text, style);
                style = Style::Heading;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut result, &mut text, style);
                style = Style::Code;
            }
            Event::Start(Tag::Item) => {
                flush(&mut result, &mut text, style);
                text.push_str("• ");
            }
            Event::End(
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Item,
            ) => {
                flush(&mut result, &mut text, style);
                style = Style::Body;
            }
            Event::Text(value) | Event::Code(value) => text.push_str(&value),
            Event::SoftBreak => text.push(if style == Style::Code { '\n' } else { ' ' }),
            Event::HardBreak => text.push('\n'),
            Event::Rule => {
                flush(&mut result, &mut text, style);
            }
            // HTML is ignored; link labels are rendered by Text events and no
            // remote image fetching or arbitrary URL activation is introduced.
            _ => {}
        }
    }
    flush(&mut result, &mut text, style);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_markdown_keeps_headings_lists_and_code_without_html_execution() {
        let blocks = blocks(
            "## 更新\n\n- 修复 **电量** 显示\n- [查看详情](https://example.com)\n\n```rust\nlet x = 1;\n```\n\n<script>bad()</script>\n",
        );
        assert_eq!(
            blocks[0],
            Block {
                text: "更新".into(),
                style: Style::Heading
            }
        );
        assert!(blocks.iter().any(|b| b.text == "• 修复 电量 显示"));
        assert!(blocks.iter().any(|b| b.text == "• 查看详情"));
        assert!(
            blocks
                .iter()
                .any(|b| b.style == Style::Code && b.text == "let x = 1;")
        );
        assert!(!blocks.iter().any(|b| b.text.contains("bad()")));
    }
}

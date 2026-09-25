use cosmic::Element;
use cosmic::iced::{Border, Color, Length};
use cosmic::widget::{button, column, container, row, text};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Clone, Debug)]
pub enum MarkdownBlock {
    Paragraph(String),
    Heading(u8, String),
    Code(Option<String>, String),
    List(Vec<String>),
    Blockquote(String),
    Rule,
}

pub fn parse_markdown(source: &str) -> Vec<MarkdownBlock> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(source, options);
    let mut blocks = Vec::new();
    let mut current_text = String::new();
    let mut current_code_lang = None;
    let mut current_heading_level = None;
    let mut in_blockquote = false;
    let mut current_list_items = Vec::new();
    let mut in_list = false;

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current_heading_level = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                });
                current_text.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(level) = current_heading_level.take() {
                    blocks.push(MarkdownBlock::Heading(
                        level,
                        current_text.trim().to_string(),
                    ));
                    current_text.clear();
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                current_code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let l = lang.trim();
                        if l.is_empty() {
                            None
                        } else {
                            Some(l.to_string())
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
                current_text.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                let lang = current_code_lang.take();
                blocks.push(MarkdownBlock::Code(lang, current_text.clone()));
                current_text.clear();
            }
            Event::Start(Tag::List(_)) => {
                in_list = true;
                current_list_items.clear();
            }
            Event::End(TagEnd::List(_)) => {
                in_list = false;
                if !current_list_items.is_empty() {
                    blocks.push(MarkdownBlock::List(std::mem::take(&mut current_list_items)));
                }
            }
            Event::Start(Tag::Item) => {
                current_text.clear();
            }
            Event::End(TagEnd::Item) => {
                if in_list && !current_text.trim().is_empty() {
                    current_list_items.push(current_text.trim().to_string());
                }
                current_text.clear();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                in_blockquote = true;
                current_text.clear();
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                in_blockquote = false;
                if !current_text.trim().is_empty() {
                    blocks.push(MarkdownBlock::Blockquote(current_text.trim().to_string()));
                }
                current_text.clear();
            }
            Event::Start(Tag::Paragraph) => {
                current_text.clear();
            }
            Event::End(TagEnd::Paragraph) => {
                if !in_list && !in_blockquote && !current_text.trim().is_empty() {
                    blocks.push(MarkdownBlock::Paragraph(current_text.trim().to_string()));
                }
                current_text.clear();
            }
            Event::Text(t) => {
                current_text.push_str(&t);
            }
            Event::Code(c) => {
                current_text.push('`');
                current_text.push_str(&c);
                current_text.push('`');
            }
            Event::SoftBreak | Event::HardBreak => {
                current_text.push('\n');
            }
            Event::Rule => {
                blocks.push(MarkdownBlock::Rule);
            }
            _ => {}
        }
    }

    if !current_text.trim().is_empty() {
        blocks.push(MarkdownBlock::Paragraph(current_text.trim().to_string()));
    }

    blocks
}

pub fn render_markdown<'a, Message: Clone + 'static, F>(
    source: &str,
    on_copy: F,
) -> Element<'a, Message>
where
    F: Fn(String) -> Message + Copy + 'static,
{
    let blocks = parse_markdown(source);
    let mut elements = Vec::with_capacity(blocks.len());

    for block in blocks {
        match block {
            MarkdownBlock::Paragraph(p) => {
                elements.push(text(p).size(14).into());
            }
            MarkdownBlock::Heading(level, h) => {
                let size = match level {
                    1 => 22,
                    2 => 18,
                    3 => 16,
                    _ => 14,
                };
                elements.push(text(h).size(size).into());
            }
            MarkdownBlock::Code(lang, code) => {
                let lang_label = lang.unwrap_or_else(|| "code".to_string());
                let header = container(
                    row::with_children(vec![
                        text(lang_label).size(11).width(Length::Fill).into(),
                        button::text("Copy")
                            .padding([2, 8])
                            .on_press(on_copy(code.clone()))
                            .into(),
                    ])
                    .align_y(cosmic::iced::Alignment::Center),
                )
                .padding([4, 10])
                .style(|_theme| container::Style {
                    background: Some(Color::from_rgb8(0x14, 0x17, 0x1a).into()),
                    border: Border {
                        color: Color::from_rgb8(0x28, 0x2c, 0x30),
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    text_color: Some(Color::from_rgb8(0x89, 0x91, 0x98)),
                    ..Default::default()
                });

                let code_text = text(code).font(cosmic::iced::Font::MONOSPACE).size(13);

                let code_container =
                    container(code_text)
                        .padding(10)
                        .width(Length::Fill)
                        .style(|_theme| container::Style {
                            text_color: Some(Color::from_rgb8(0xe1, 0xdd, 0xd5)),
                            ..Default::default()
                        });

                let block_col = column::with_children(vec![header.into(), code_container.into()]);

                let code_block_container =
                    container(block_col)
                        .width(Length::Fill)
                        .style(|_theme| container::Style {
                            background: Some(Color::from_rgb8(0x17, 0x1a, 0x1d).into()),
                            border: Border {
                                color: Color::from_rgb8(0x30, 0x35, 0x3a),
                                width: 1.0,
                                radius: 6.0.into(),
                            },
                            ..Default::default()
                        });

                elements.push(code_block_container.into());
            }
            MarkdownBlock::List(items) => {
                let mut list_col = column::with_capacity(items.len()).spacing(4);
                for item in items {
                    let bullet_item = row::with_children(vec![
                        text("• ").size(14).into(),
                        text(item).size(14).width(Length::Fill).into(),
                    ]);
                    list_col = list_col.push(bullet_item);
                }
                elements.push(container(list_col).padding([2, 8]).into());
            }
            MarkdownBlock::Blockquote(quote) => {
                let q = container(text(quote).size(13))
                    .padding([6, 12])
                    .style(|_theme| container::Style {
                        background: Some(Color::from_rgba8(255, 255, 255, 0.02).into()),
                        border: Border {
                            color: Color::from_rgb8(0x6f, 0x77, 0x80),
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        text_color: Some(Color::from_rgb8(0xbc, 0xc1, 0xc4)),
                        ..Default::default()
                    });
                elements.push(q.into());
            }
            MarkdownBlock::Rule => {
                let rule = container(text(""))
                    .height(1)
                    .width(Length::Fill)
                    .style(|_theme| container::Style {
                        background: Some(Color::from_rgb8(0x28, 0x2c, 0x30).into()),
                        ..Default::default()
                    });
                elements.push(rule.into());
            }
        }
    }

    column::with_children(elements).spacing(8).into()
}

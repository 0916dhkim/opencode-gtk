use cosmic::Element;
use cosmic::iced::{Border, Length};
use cosmic::widget::{button, column, container, row, text};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::metrics::{em, space};
use crate::palette;

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
    zoom: f32,
) -> Element<'a, Message>
where
    F: Fn(String) -> Message + Copy + 'static,
{
    let blocks = parse_markdown(source);
    let mut elements = Vec::with_capacity(blocks.len());

    for block in blocks {
        match block {
            MarkdownBlock::Paragraph(p) => {
                elements.push(text(p).size(em(0.96, zoom)).into());
            }
            MarkdownBlock::Heading(level, h) => {
                // GTK: .markdown-heading-1/2/3 = 1.45 / 1.28 / 1.14em.
                let size = match level {
                    1 => em(1.45, zoom),
                    2 => em(1.28, zoom),
                    3 => em(1.14, zoom),
                    _ => em(0.96, zoom),
                };
                elements.push(
                    text(h)
                        .size(size)
                        .class(cosmic::theme::Text::Color(
                            palette::current().header_title_text,
                        ))
                        .into(),
                );
            }
            MarkdownBlock::Code(lang, code) => {
                let lang_label = lang.unwrap_or_else(|| "code".to_string());
                let header = container(
                    row::with_children(vec![
                        text(lang_label)
                            .size(em(0.76, zoom))
                            .font(cosmic::iced::Font {
                                weight: cosmic::iced::font::Weight::Bold,
                                ..cosmic::iced::Font::DEFAULT
                            })
                            .width(Length::Fill)
                            .into(),
                        button::icon(crate::icons::copy())
                            .padding([2, 6])
                            .on_press(on_copy(code.clone()))
                            .into(),
                    ])
                    .align_y(cosmic::iced::Alignment::Center),
                )
                .padding([space(0.3, zoom) as u16, space(0.59, zoom) as u16])
                .style(|_theme| container::Style {
                    background: Some(palette::current().code_header_bg.into()),
                    border: Border {
                        color: palette::current().panel_border,
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    text_color: Some(palette::current().code_language_text),
                    ..Default::default()
                });

                let code_text = text(code)
                    .font(cosmic::iced::Font::MONOSPACE)
                    .size(em(0.92, zoom));

                let code_container = container(code_text)
                    .padding([space(0.59, zoom) as u16, space(0.74, zoom) as u16])
                    .width(Length::Fill)
                    .style(|_theme| container::Style {
                        text_color: Some(palette::current().code_content_text),
                        ..Default::default()
                    });

                let block_col = column::with_children(vec![header.into(), code_container.into()]);

                let code_block_container =
                    container(block_col)
                        .width(Length::Fill)
                        .style(|_theme| container::Style {
                            background: Some(palette::current().code_block_bg.into()),
                            border: Border {
                                color: palette::current().code_block_border,
                                width: 1.0,
                                radius: 6.0.into(),
                            },
                            ..Default::default()
                        });

                elements.push(code_block_container.into());
            }
            MarkdownBlock::List(items) => {
                let mut list_col = column::with_capacity(items.len()).spacing(space(0.3, zoom));
                for item in items {
                    let bullet_item = row::with_children(vec![
                        text("• ")
                            .size(em(0.96, zoom))
                            .class(cosmic::theme::Text::Color(palette::current().muted_text))
                            .into(),
                        text(item).size(em(0.96, zoom)).width(Length::Fill).into(),
                    ]);
                    list_col = list_col.push(bullet_item);
                }
                elements.push(
                    container(list_col)
                        .padding([space(0.15, zoom) as u16, space(0.59, zoom) as u16])
                        .into(),
                );
            }
            MarkdownBlock::Blockquote(quote) => {
                let q = container(text(quote).size(em(0.96, zoom)))
                    .padding([space(0.4, zoom) as u16, space(0.7, zoom) as u16])
                    .style(|_theme| container::Style {
                        background: Some(palette::current().overlay_bg.into()),
                        border: Border {
                            color: palette::current().quote_border,
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        text_color: Some(palette::current().quote_text),
                        ..Default::default()
                    });
                elements.push(q.into());
            }
            MarkdownBlock::Rule => {
                let rule = container(text(""))
                    .height(1)
                    .width(Length::Fill)
                    .style(|_theme| container::Style {
                        background: Some(palette::current().panel_border.into()),
                        ..Default::default()
                    });
                elements.push(rule.into());
            }
        }
    }

    column::with_children(elements).spacing(8).into()
}

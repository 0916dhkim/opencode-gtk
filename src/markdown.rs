use gtk::{glib, pango, prelude::*};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading(u8),
    Code(Option<String>),
    Rule,
    Table(TableBlock),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellAlign {
    Default,
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TableBlock {
    alignments: Vec<CellAlign>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Block {
    kind: BlockKind,
    content: String,
    marker: Option<String>,
    list_depth: usize,
    quote_depth: usize,
}

#[derive(Clone, Debug)]
struct ListState {
    next: Option<u64>,
}

#[derive(Clone, Debug)]
struct OpenBlock {
    kind: BlockKind,
    content: String,
    marker: Option<String>,
    list_depth: usize,
    quote_depth: usize,
}

#[derive(Default)]
struct MarkdownParser {
    blocks: Vec<Block>,
    current: Option<OpenBlock>,
    code: Option<OpenBlock>,
    table: Option<OpenTable>,
    lists: Vec<ListState>,
    pending_marker: Option<String>,
    quote_depth: usize,
    link_markup: Vec<bool>,
}

#[derive(Clone, Debug)]
struct OpenTable {
    alignments: Vec<CellAlign>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
}

pub fn render_into(container: &gtk::Box, source: &str) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    for block in parse(source) {
        append_block(container, block);
    }
}

fn parse(source: &str) -> Vec<Block> {
    let mut options = Options::empty();
    options
        .insert(Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES);
    let mut state = MarkdownParser::default();

    for event in Parser::new_ext(source, options) {
        if state.code.is_some() {
            match event {
                Event::End(TagEnd::CodeBlock) => state.finish_code(),
                Event::Text(text)
                | Event::Code(text)
                | Event::Html(text)
                | Event::InlineHtml(text) => state.append_code(&text),
                Event::SoftBreak | Event::HardBreak => state.append_code("\n"),
                _ => {}
            }
            continue;
        }

        match event {
            Event::Start(tag) => state.start(tag),
            Event::End(tag) => state.end(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                state.append_escaped(&text)
            }
            Event::Code(text) => {
                state.append_markup("<span font_family=\"monospace\">");
                state.append_escaped(&text);
                state.append_markup("</span>");
            }
            Event::InlineMath(text) => {
                state.append_markup("<i>");
                state.append_escaped(&text);
                state.append_markup("</i>");
            }
            Event::DisplayMath(text) => {
                state.finish_current();
                state.begin(BlockKind::Code(Some("math".to_owned())));
                state.append_markup(&text);
                state.finish_current();
            }
            Event::FootnoteReference(label) => {
                state.append_escaped(&format!("[{label}]"));
            }
            Event::SoftBreak => state.append_markup("\n"),
            Event::HardBreak => state.append_markup("\n"),
            Event::Rule => {
                state.finish_current();
                state.begin(BlockKind::Rule);
                state.finish_current();
            }
            Event::TaskListMarker(checked) => {
                state.append_markup(if checked { "[x] " } else { "[ ] " });
            }
        }
    }

    state.finish_code();
    state.finish_table();
    state.finish_current();
    state.blocks
}

impl MarkdownParser {
    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.begin(BlockKind::Paragraph),
            Tag::Heading { level, .. } => self.begin(BlockKind::Heading(heading_level(level))),
            Tag::BlockQuote(_) => {
                self.finish_current();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.finish_current();
                let language = match kind {
                    CodeBlockKind::Indented => None,
                    CodeBlockKind::Fenced(language) => {
                        let language = language.trim();
                        (!language.is_empty()).then(|| language.to_owned())
                    }
                };
                self.code = Some(self.open_block(BlockKind::Code(language)));
            }
            Tag::List(start) => {
                self.finish_current();
                self.lists.push(ListState { next: start });
            }
            Tag::Item => {
                self.finish_current();
                let marker = self
                    .lists
                    .last_mut()
                    .map(|list| match list.next.as_mut() {
                        Some(next) => {
                            let marker = format!("{next}.");
                            *next += 1;
                            marker
                        }
                        None => "•".to_owned(),
                    })
                    .unwrap_or_else(|| "•".to_owned());
                self.pending_marker = Some(marker);
            }
            Tag::Emphasis => self.append_markup("<i>"),
            Tag::Strong => self.append_markup("<b>"),
            Tag::Strikethrough => self.append_markup("<span strikethrough=\"true\">"),
            Tag::Superscript => self.append_markup("<sup>"),
            Tag::Subscript => self.append_markup("<sub>"),
            Tag::Link { dest_url, .. } => {
                let safe = safe_link(&dest_url);
                if safe {
                    self.append_markup("<a href=\"");
                    self.append_escaped(&dest_url);
                    self.append_markup("\">");
                }
                self.link_markup.push(safe);
            }
            Tag::Image { .. } => self.append_markup("Image: "),
            Tag::Table(alignments) => {
                self.finish_current();
                self.table = Some(OpenTable {
                    alignments: alignments.iter().copied().map(cell_align).collect(),
                    header: Vec::new(),
                    rows: Vec::new(),
                    current_row: Vec::new(),
                });
            }
            Tag::TableHead | Tag::TableRow => {}
            Tag::TableCell => {
                self.current = Some(self.open_block(BlockKind::Paragraph));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.finish_current(),
            TagEnd::BlockQuote(_) => {
                self.finish_current();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.finish_current();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.finish_current();
                self.pending_marker = None;
            }
            TagEnd::Emphasis => self.append_markup("</i>"),
            TagEnd::Strong => self.append_markup("</b>"),
            TagEnd::Strikethrough => self.append_markup("</span>"),
            TagEnd::Superscript => self.append_markup("</sup>"),
            TagEnd::Subscript => self.append_markup("</sub>"),
            TagEnd::Link => {
                if self.link_markup.pop().unwrap_or(false) {
                    self.append_markup("</a>");
                }
            }
            TagEnd::TableCell => self.finish_table_cell(),
            TagEnd::TableRow | TagEnd::TableHead => self.finish_table_row(),
            TagEnd::Table => self.finish_table(),
            _ => {}
        }
    }

    fn begin(&mut self, kind: BlockKind) {
        self.finish_current();
        self.current = Some(self.open_block(kind));
    }

    fn open_block(&mut self, kind: BlockKind) -> OpenBlock {
        OpenBlock {
            kind,
            content: String::new(),
            marker: self.pending_marker.take(),
            list_depth: self.lists.len(),
            quote_depth: self.quote_depth,
        }
    }

    fn ensure_current(&mut self) {
        if self.current.is_none() {
            self.current = Some(self.open_block(BlockKind::Paragraph));
        }
    }

    fn append_markup(&mut self, text: &str) {
        self.ensure_current();
        if let Some(current) = &mut self.current {
            current.content.push_str(text);
        }
    }

    fn append_escaped(&mut self, text: &str) {
        self.append_markup(&glib::markup_escape_text(text));
    }

    fn append_code(&mut self, text: &str) {
        if let Some(code) = &mut self.code {
            code.content.push_str(text);
        }
    }

    fn finish_current(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        if current.content.is_empty() && current.kind != BlockKind::Rule {
            return;
        }
        self.blocks.push(Block {
            kind: current.kind,
            content: current.content,
            marker: current.marker,
            list_depth: current.list_depth,
            quote_depth: current.quote_depth,
        });
    }

    fn finish_code(&mut self) {
        let Some(code) = self.code.take() else {
            return;
        };
        self.blocks.push(Block {
            kind: code.kind,
            content: code.content,
            marker: code.marker,
            list_depth: code.list_depth,
            quote_depth: code.quote_depth,
        });
    }

    fn finish_table_cell(&mut self) {
        let content = self
            .current
            .take()
            .map(|block| block.content)
            .unwrap_or_default();
        if let Some(table) = &mut self.table {
            table.current_row.push(content);
        }
    }

    fn finish_table_row(&mut self) {
        if self.current.is_some() {
            self.finish_table_cell();
        }
        let Some(table) = &mut self.table else {
            return;
        };
        let row = std::mem::take(&mut table.current_row);
        if row.is_empty() {
            return;
        }
        if table.header.is_empty() {
            table.header = row;
        } else {
            table.rows.push(row);
        }
    }

    fn finish_table(&mut self) {
        if self.table.is_none() {
            return;
        }
        self.finish_table_row();
        let Some(table) = self.table.take() else {
            return;
        };
        if table.header.is_empty() && table.rows.is_empty() {
            return;
        }
        self.blocks.push(Block {
            kind: BlockKind::Table(TableBlock {
                alignments: table.alignments,
                header: table.header,
                rows: table.rows,
            }),
            content: String::new(),
            marker: None,
            list_depth: self.lists.len(),
            quote_depth: self.quote_depth,
        });
    }
}

fn append_block(container: &gtk::Box, block: Block) {
    let marker = block.marker.clone();
    let list_depth = block.list_depth;
    let quote_depth = block.quote_depth;
    let content = block_widget(block);
    content.set_hexpand(true);

    let aligned = if let Some(marker) = marker {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        let marker = gtk::Label::new(Some(&marker));
        marker.set_xalign(1.0);
        marker.set_yalign(0.0);
        marker.add_css_class("markdown-list-marker");
        row.append(&marker);
        row.append(&content);
        row.upcast::<gtk::Widget>()
    } else {
        if list_depth > 0 {
            content.set_margin_start(28);
        }
        content
    };
    aligned.set_margin_start((list_depth.saturating_sub(1) * 22) as i32);

    if quote_depth > 0 {
        let quote = gtk::Box::new(gtk::Orientation::Vertical, 0);
        quote.add_css_class("markdown-blockquote");
        quote.set_margin_start((quote_depth.saturating_sub(1) * 14) as i32);
        quote.append(&aligned);
        container.append(&quote);
    } else {
        container.append(&aligned);
    }
}

fn block_widget(block: Block) -> gtk::Widget {
    let Block { kind, content, .. } = block;
    match kind {
        BlockKind::Paragraph => rich_label(&content, "markdown-paragraph").upcast(),
        BlockKind::Heading(level) => {
            let label = rich_label(&content, "markdown-heading");
            label.add_css_class(&format!("markdown-heading-{level}"));
            label.upcast()
        }
        BlockKind::Code(language) => {
            let code_block = gtk::Box::new(gtk::Orientation::Vertical, 4);
            code_block.add_css_class("markdown-code-block");
            if let Some(language) = language {
                let language = gtk::Label::new(Some(&language));
                language.set_xalign(0.0);
                language.add_css_class("markdown-code-language");
                code_block.append(&language);
            }
            let code = gtk::Label::new(Some(&content));
            code.set_xalign(0.0);
            code.set_yalign(0.0);
            code.set_selectable(true);
            code.set_wrap(false);
            code.add_css_class("markdown-code-content");
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .vscrollbar_policy(gtk::PolicyType::Never)
                .child(&code)
                .build();
            code_block.append(&scroll);
            code_block.upcast()
        }
        BlockKind::Rule => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
        BlockKind::Table(table) => table_widget(table),
    }
}

fn table_widget(table: TableBlock) -> gtk::Widget {
    let columns = table_column_count(&table);
    let grid = gtk::Grid::new();
    grid.add_css_class("markdown-table");
    grid.set_column_spacing(0);
    grid.set_row_spacing(0);
    let mut row_index = 0;
    if !table.header.is_empty() {
        attach_table_row(&grid, &table.header, &table.alignments, columns, 0, true);
        row_index = 1;
    }
    for row in table.rows {
        attach_table_row(&grid, &row, &table.alignments, columns, row_index, false);
        row_index += 1;
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .overlay_scrolling(false)
        .propagate_natural_height(true)
        .propagate_natural_width(false)
        .hexpand(true)
        .child(&grid)
        .build();
    scroll.add_css_class("markdown-table-scroll");
    scroll.upcast()
}

fn attach_table_row(
    grid: &gtk::Grid,
    cells: &[String],
    alignments: &[CellAlign],
    columns: usize,
    row: i32,
    header: bool,
) {
    for column in 0..columns {
        let markup = cells.get(column).map(String::as_str).unwrap_or("");
        let class = if header {
            "markdown-table-header"
        } else {
            "markdown-table-cell"
        };
        let label = rich_label(markup, class);
        label.set_wrap(false);
        label.set_xalign(cell_xalign(
            alignments
                .get(column)
                .copied()
                .unwrap_or(CellAlign::Default),
        ));
        if column + 1 == columns {
            label.add_css_class("markdown-table-last");
        }
        grid.attach(&label, column as i32, row, 1, 1);
    }
}

fn table_column_count(table: &TableBlock) -> usize {
    table
        .alignments
        .len()
        .max(table.header.len())
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0))
}

fn cell_align(alignment: Alignment) -> CellAlign {
    match alignment {
        Alignment::None => CellAlign::Default,
        Alignment::Left => CellAlign::Left,
        Alignment::Center => CellAlign::Center,
        Alignment::Right => CellAlign::Right,
    }
}

fn cell_xalign(align: CellAlign) -> f32 {
    match align {
        CellAlign::Default | CellAlign::Left => 0.0,
        CellAlign::Center => 0.5,
        CellAlign::Right => 1.0,
    }
}

fn rich_label(markup: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_markup(markup);
    label.set_xalign(0.0);
    label.set_yalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.add_css_class(class);
    label
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn safe_link(target: &str) -> bool {
    Url::parse(target).is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_markdown_blocks_and_inline_styles() {
        let blocks = parse(
            "# Heading\n\nA *small* **strong** [`link`](https://example.com?a=1&b=2).\n\n- first\n- second\n\n> quoted\n\n```rust\nfn main() {}\n```",
        );

        assert_eq!(blocks[0].kind, BlockKind::Heading(1));
        assert_eq!(blocks[0].content, "Heading");
        assert!(blocks[1].content.contains("<i>small</i>"));
        assert!(blocks[1].content.contains("<b>strong</b>"));
        assert!(blocks[1]
            .content
            .contains("<a href=\"https://example.com?a=1&amp;b=2\">"));
        assert_eq!(blocks[2].marker.as_deref(), Some("•"));
        assert_eq!(blocks[3].marker.as_deref(), Some("•"));
        assert_eq!(blocks[4].quote_depth, 1);
        assert_eq!(blocks[5].kind, BlockKind::Code(Some("rust".to_owned())));
        assert_eq!(blocks[5].content, "fn main() {}\n");
    }

    #[test]
    fn escapes_markup_and_rejects_unsafe_links() {
        let blocks = parse(
            "<span foreground=\"red\">unsafe</span> and [file](file:///etc/passwd) with `</span>`",
        );
        let markup = &blocks[0].content;

        assert!(markup.contains("&lt;span foreground=&quot;red&quot;&gt;"));
        assert!(!markup.contains("href="));
        assert!(markup.contains("&lt;/span&gt;"));
    }

    #[test]
    fn incomplete_streaming_markdown_remains_renderable() {
        assert_eq!(parse("**partial")[0].content, "**partial");
        let blocks = parse("```rust\nfn main(");
        assert_eq!(blocks[0].kind, BlockKind::Code(Some("rust".to_owned())));
        assert_eq!(blocks[0].content, "fn main(");
    }

    #[test]
    fn link_policy_allows_only_browser_safe_schemes() {
        assert!(safe_link("https://example.com"));
        assert!(safe_link("http://localhost:4096"));
        assert!(safe_link("mailto:test@example.com"));
        assert!(!safe_link("file:///etc/passwd"));
        assert!(!safe_link("javascript:alert(1)"));
        assert!(!safe_link("not a url"));
    }

    #[test]
    fn parses_markdown_tables_with_alignment_and_inline_markup() {
        let blocks = parse("| Name | Count |\n| :--- | ---: |\n| *alpha* | 2 |\n| beta | 3 |\n");
        let BlockKind::Table(table) = &blocks[0].kind else {
            panic!("expected table, got {:?}", blocks[0].kind);
        };
        assert_eq!(table.alignments, vec![CellAlign::Left, CellAlign::Right]);
        assert_eq!(table.header, ["Name", "Count"]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][0], "<i>alpha</i>");
        assert_eq!(table.rows[0][1], "2");
        assert_eq!(table.rows[1], ["beta", "3"]);
    }

    #[test]
    fn incomplete_table_stays_renderable() {
        assert!(!parse("| Name | Count |\n| ---").is_empty());
    }
}

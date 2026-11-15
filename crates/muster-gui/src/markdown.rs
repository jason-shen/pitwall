//! A Markdown preview drawn with egui, for the live view beside the editor.
//! It records where each block of the source landed so the editor and the
//! preview can scroll together.

use egui::text::LayoutJob;
use egui::{pos2, vec2, Color32, FontId, Rect, Sense, Stroke, TextFormat, Ui};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::theme::c;

/// A source line and the y offset (from the top of the preview's content)
/// where the block starting on it was drawn.
pub type Anchors = Vec<(usize, f32)>;

const BODY: f32 = 14.5;

#[derive(Clone, Copy)]
enum Marker {
    Bullet,
    Number(u64),
    Task(bool),
}

struct Style {
    strong: bool,
    italic: bool,
    strike: bool,
    link: bool,
    heading: Option<u8>,
}

impl Style {
    fn format(&self, code: bool) -> TextFormat {
        let size = match self.heading {
            Some(1) => 26.0,
            Some(2) => 21.0,
            Some(3) => 17.5,
            Some(_) => 15.5,
            None => BODY,
        };
        let mut f = if code {
            TextFormat {
                font_id: FontId::monospace(size - 1.5),
                color: c::WARNING,
                background: c::CARD,
                ..Default::default()
            }
        } else {
            TextFormat::simple(FontId::proportional(size), c::TEXT)
        };
        // There's no bold proportional face, so strong text and headings
        // stand out by brightness instead.
        if self.strong || self.heading.is_some() {
            f.color = Color32::WHITE;
        }
        if self.link {
            f.color = c::ACCENT;
            f.underline = Stroke::new(1.0, c::ACCENT.gamma_multiply(0.6));
        }
        f.italics = self.italic;
        if self.strike {
            f.strikethrough = Stroke::new(1.0, c::TEXT_DIM);
            f.color = c::TEXT_DIM;
        }
        f
    }
}

/// Draw `src` and return where each block landed.
pub fn show(ui: &mut Ui, src: &str) -> Anchors {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(src.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_of = |off: usize| line_starts.partition_point(|&s| s <= off).saturating_sub(1);
    let top = ui.min_rect().top();
    let mut anchors: Anchors = vec![];

    let opts = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut st = Style {
        strong: false,
        italic: false,
        strike: false,
        link: false,
        heading: None,
    };
    let mut job = LayoutJob::default();
    let mut lists: Vec<Option<u64>> = vec![];
    let mut marker: Option<Marker> = None;
    let mut quote = 0usize;
    let mut code: Option<String> = None;
    let mut table: Option<Vec<Vec<LayoutJob>>> = None;
    let mut cell: Option<LayoutJob> = None;
    let mut blocks = 0usize;

    let flush = |ui: &mut Ui,
                 job: &mut LayoutJob,
                 marker: &mut Option<Marker>,
                 depth: usize,
                 quote: usize| {
        if job.text.is_empty() && marker.is_none() {
            return;
        }
        let job = std::mem::take(job);
        let indent = depth as f32 * 20.0 + quote as f32 * 14.0;
        let r = ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(indent);
            if depth > 0 {
                let (gutter, _) = ui.allocate_exact_size(vec2(20.0, BODY + 4.0), Sense::hover());
                if let Some(m) = marker.take() {
                    draw_marker(ui, gutter, m);
                }
            }
            ui.add(egui::Label::new(job).wrap());
        });
        if quote > 0 {
            let rr = r.response.rect;
            for q in 0..quote {
                let x = rr.left() + 4.0 + q as f32 * 14.0;
                ui.painter().line_segment(
                    [pos2(x, rr.top()), pos2(x, rr.bottom())],
                    Stroke::new(3.0, c::BORDER),
                );
            }
        }
        ui.add_space(4.0);
    };

    for (ev, range) in Parser::new_ext(src, opts).into_offset_iter() {
        match ev {
            Event::Start(tag) => {
                let block = matches!(
                    tag,
                    Tag::Paragraph
                        | Tag::Heading { .. }
                        | Tag::Item
                        | Tag::CodeBlock(_)
                        | Tag::Table(_)
                        | Tag::BlockQuote(_)
                );
                if block {
                    anchors.push((line_of(range.start), ui.cursor().top() - top));
                }
                match tag {
                    Tag::Heading { level, .. } => {
                        flush(ui, &mut job, &mut marker, lists.len(), quote);
                        ui.add_space(if (level as u8) <= 2 { 10.0 } else { 6.0 });
                        st.heading = Some(level as u8);
                    }
                    Tag::Paragraph => {}
                    Tag::List(start) => {
                        // A nested list ends the parent item's text.
                        flush(ui, &mut job, &mut marker, lists.len(), quote);
                        lists.push(start);
                    }
                    Tag::Item => {
                        flush(ui, &mut job, &mut marker, lists.len(), quote);
                        marker = Some(match lists.last_mut() {
                            Some(Some(n)) => {
                                let m = Marker::Number(*n);
                                *n += 1;
                                m
                            }
                            _ => Marker::Bullet,
                        });
                    }
                    Tag::BlockQuote(_) => {
                        flush(ui, &mut job, &mut marker, lists.len(), quote);
                        quote += 1;
                    }
                    Tag::CodeBlock(_) => {
                        flush(ui, &mut job, &mut marker, lists.len(), quote);
                        code = Some(String::new());
                    }
                    Tag::Table(_) => {
                        flush(ui, &mut job, &mut marker, lists.len(), quote);
                        table = Some(vec![]);
                    }
                    Tag::TableHead | Tag::TableRow => {
                        if let Some(t) = table.as_mut() {
                            t.push(vec![]);
                        }
                    }
                    Tag::TableCell => cell = Some(LayoutJob::default()),
                    Tag::Strong => st.strong = true,
                    Tag::Emphasis => st.italic = true,
                    Tag::Strikethrough => st.strike = true,
                    Tag::Link { .. } => st.link = true,
                    Tag::Image { .. } => st.italic = true,
                    _ => {}
                }
            }
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    flush(ui, &mut job, &mut marker, lists.len(), quote);
                    st.heading = None;
                    ui.add_space(2.0);
                }
                TagEnd::Paragraph => {
                    flush(ui, &mut job, &mut marker, lists.len(), quote);
                    if lists.is_empty() {
                        ui.add_space(4.0);
                    }
                }
                TagEnd::Item => flush(ui, &mut job, &mut marker, lists.len(), quote),
                TagEnd::List(_) => {
                    flush(ui, &mut job, &mut marker, lists.len(), quote);
                    lists.pop();
                    if lists.is_empty() {
                        ui.add_space(6.0);
                    }
                }
                TagEnd::BlockQuote(_) => {
                    flush(ui, &mut job, &mut marker, lists.len(), quote);
                    quote = quote.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    let text = code.take().unwrap_or_default();
                    blocks += 1;
                    code_block(ui, text.trim_end_matches('\n'), lists.len(), blocks);
                }
                TagEnd::TableCell => {
                    if let (Some(t), Some(cl)) = (table.as_mut(), cell.take()) {
                        if let Some(row) = t.last_mut() {
                            row.push(cl);
                        }
                    }
                }
                TagEnd::Table => {
                    blocks += 1;
                    draw_table(ui, table.take().unwrap_or_default(), blocks);
                }
                TagEnd::Strong => st.strong = false,
                TagEnd::Emphasis => st.italic = false,
                TagEnd::Strikethrough => st.strike = false,
                TagEnd::Link => st.link = false,
                TagEnd::Image => st.italic = false,
                _ => {}
            },
            // An empty `- [ ]` isn't a task to CommonMark, but in a fresh
            // template it's meant as one.
            Event::Text(t)
                if job.text.is_empty()
                    && matches!(marker, Some(Marker::Bullet))
                    && matches!(t.trim(), "[ ]" | "[x]" | "[X]") =>
            {
                marker = Some(Marker::Task(t.trim() != "[ ]"));
            }
            Event::Text(t) => {
                if let Some(code) = code.as_mut() {
                    code.push_str(&t);
                } else if let Some(cl) = cell.as_mut() {
                    cl.append(&t, 0.0, st.format(false));
                } else {
                    job.append(&t, 0.0, st.format(false));
                }
            }
            Event::Code(t) => {
                let target = cell.as_mut().unwrap_or(&mut job);
                target.append(&t, 0.0, st.format(true));
            }
            Event::SoftBreak => {
                let target = cell.as_mut().unwrap_or(&mut job);
                target.append(" ", 0.0, st.format(false));
            }
            Event::HardBreak => job.append("\n", 0.0, st.format(false)),
            Event::TaskListMarker(done) => marker = Some(Marker::Task(done)),
            Event::Rule => {
                flush(ui, &mut job, &mut marker, lists.len(), quote);
                anchors.push((line_of(range.start), ui.cursor().top() - top));
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
            }
            // HTML, including the guide comments in briefs, isn't shown.
            _ => {}
        }
    }
    flush(ui, &mut job, &mut marker, lists.len(), quote);
    anchors
}

fn draw_marker(ui: &Ui, gutter: Rect, m: Marker) {
    let p = ui.painter();
    let y = gutter.top() + BODY * 0.62;
    match m {
        Marker::Bullet => {
            p.circle_filled(pos2(gutter.left() + 7.0, y), 2.5, c::TEXT_DIM);
        }
        Marker::Number(n) => {
            p.text(
                pos2(gutter.right() - 4.0, y),
                egui::Align2::RIGHT_CENTER,
                format!("{n}."),
                FontId::proportional(BODY - 1.0),
                c::TEXT_DIM,
            );
        }
        Marker::Task(done) => {
            let b = Rect::from_center_size(pos2(gutter.left() + 7.0, y), vec2(12.0, 12.0));
            if done {
                p.rect_filled(b, 3.0, c::ACCENT);
                p.line_segment(
                    [
                        b.left_center() + vec2(2.5, 0.0),
                        b.center_bottom() + vec2(-1.0, -3.0),
                    ],
                    Stroke::new(1.8, c::BG),
                );
                p.line_segment(
                    [
                        b.center_bottom() + vec2(-1.0, -3.0),
                        b.right_top() + vec2(-2.5, 3.0),
                    ],
                    Stroke::new(1.8, c::BG),
                );
            } else {
                p.rect_stroke(
                    b,
                    3.0,
                    Stroke::new(1.3, c::TEXT_DIM),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }
}

fn code_block(ui: &mut Ui, text: &str, depth: usize, n: usize) {
    ui.horizontal_top(|ui| {
        ui.add_space(depth as f32 * 20.0);
        egui::Frame::new()
            .fill(c::CARD)
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::horizontal()
                    .id_salt(("md-code", n))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text)
                                    .font(FontId::monospace(BODY - 1.5))
                                    .color(c::TEXT),
                            )
                            .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            });
    });
    ui.add_space(8.0);
}

fn draw_table(ui: &mut Ui, rows: Vec<Vec<LayoutJob>>, n: usize) {
    // Cells don't wrap, so a wide table scrolls sideways like a code block.
    egui::ScrollArea::horizontal()
        .id_salt(("md-table-scroll", n))
        .show(ui, |ui| {
            egui::Frame::new()
                .stroke(Stroke::new(1.0, c::BORDER))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    egui::Grid::new(("md-table", n))
                        .striped(true)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            for (i, row) in rows.into_iter().enumerate() {
                                for mut cell in row {
                                    if i == 0 {
                                        for s in cell.sections.iter_mut() {
                                            s.format.color = Color32::WHITE;
                                        }
                                    }
                                    ui.add(
                                        egui::Label::new(cell)
                                            .wrap_mode(egui::TextWrapMode::Extend),
                                    );
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    ui.add_space(8.0);
}

/// Where in the preview a source line is, between the anchors around it.
pub fn y_for_line(anchors: &Anchors, line: usize) -> f32 {
    let i = anchors.partition_point(|a| a.0 <= line);
    match (i.checked_sub(1).map(|j| anchors[j]), anchors.get(i)) {
        (Some((l0, y0)), Some(&(l1, y1))) if l1 > l0 => {
            y0 + (y1 - y0) * (line - l0) as f32 / (l1 - l0) as f32
        }
        (Some((_, y0)), _) => y0,
        (None, _) => 0.0,
    }
}

/// The source line at a point in the preview: the inverse of `y_for_line`.
pub fn line_for_y(anchors: &Anchors, y: f32) -> usize {
    let i = anchors.partition_point(|a| a.1 <= y);
    match (i.checked_sub(1).map(|j| anchors[j]), anchors.get(i)) {
        (Some((l0, y0)), Some(&(l1, y1))) if y1 > y0 => {
            l0 + ((l1 - l0) as f32 * (y - y0) / (y1 - y0)).round() as usize
        }
        (Some((l0, _)), _) => l0,
        (None, _) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_positions_map_both_ways() {
        let a: Anchors = vec![(0, 0.0), (10, 100.0), (20, 400.0)];
        assert_eq!(y_for_line(&a, 5), 50.0);
        assert_eq!(y_for_line(&a, 15), 250.0);
        assert_eq!(y_for_line(&a, 30), 400.0);
        assert_eq!(line_for_y(&a, 50.0), 5);
        assert_eq!(line_for_y(&a, 250.0), 15);
        assert_eq!(line_for_y(&a, 1000.0), 20);
        assert_eq!(line_for_y(&vec![], 10.0), 0);
    }
}

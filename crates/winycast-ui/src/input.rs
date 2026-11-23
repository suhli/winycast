use std::ops::Range;

use gpui::prelude::FluentBuilder;
use gpui::*;
use unicode_segmentation::UnicodeSegmentation;

use crate::Intent;

pub struct QueryChanged(pub String);
impl EventEmitter<QueryChanged> for SearchInput {}

pub struct SearchInput {
    pub focus: FocusHandle,
    intents: async_channel::Sender<Intent>,
    text: String,
    anchor: usize,
    caret: usize,
    composition: Option<Composition>,
    geometry: Option<Geometry>,
    placeholder: SharedString,
    foreground: Hsla,
    muted: Hsla,
    accent: Hsla,
    dragging: bool,
    layout: InputLayout,
}

struct InputLayout {
    font_size: f32,
    line_height: f32,
    rows: usize,
    limit: usize,
    grow: bool,
}

struct Composition {
    range: Range<usize>,
    original: String,
}

struct Geometry {
    lines: Vec<VisibleLine>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
}

struct VisibleLine {
    line: ShapedLine,
    range: Range<usize>,
    offset: Pixels,
}

impl SearchInput {
    pub fn new(intents: async_channel::Sender<Intent>, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            intents,
            text: String::new(),
            anchor: 0,
            caret: 0,
            composition: None,
            geometry: None,
            placeholder: "搜索应用、窗口、命令…".into(),
            foreground: rgb(0xf2f5f8).into(),
            muted: rgb(0x9ca7b6).into(),
            accent: rgb(0x62dccc).into(),
            dragging: false,
            layout: InputLayout {
                font_size: 22.,
                line_height: 34.,
                rows: 1,
                limit: 8192,
                grow: true,
            },
        }
    }

    pub fn shortcut_field(
        intents: async_channel::Sender<Intent>,
        multiline: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = Self::new(intents, cx);
        input.layout = if multiline {
            InputLayout {
                font_size: 16.,
                line_height: 25.,
                rows: 5,
                limit: 8192,
                grow: false,
            }
        } else {
            InputLayout {
                font_size: 16.,
                line_height: 34.,
                rows: 1,
                limit: 256,
                grow: false,
            }
        };
        input
    }

    pub fn note_body(intents: async_channel::Sender<Intent>, cx: &mut Context<Self>) -> Self {
        let mut input = Self::shortcut_field(intents, true, cx);
        input.layout.rows = 8;
        input.layout.line_height = 23.;
        input.layout.limit = winycast_core::MARKDOWN_LIMIT;
        input
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn composing(&self) -> bool {
        self.composition.is_some()
    }

    pub fn cancel_composition(&mut self, cx: &mut Context<Self>) {
        if let Some(composition) = self.composition.take() {
            let start = composition.range.start;
            let end = start + composition.original.len();
            self.text
                .replace_range(composition.range, &composition.original);
            self.anchor = start;
            self.caret = end;
            cx.notify();
        }
    }

    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.replace(0..self.text.len(), &text, cx);
    }

    pub fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.composing() {
            self.replace(self.selection(), text, cx);
        }
    }

    pub fn style(
        &mut self,
        placeholder: &'static str,
        foreground: Hsla,
        muted: Hsla,
        accent: Hsla,
    ) {
        self.placeholder = placeholder.into();
        self.foreground = foreground;
        self.muted = muted;
        self.accent = accent;
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    fn set_caret(&mut self, position: usize, extend: bool, cx: &mut Context<Self>) {
        self.caret = position;
        if !extend {
            self.anchor = position;
        }
        cx.notify();
    }

    fn previous(&self, at: usize) -> usize {
        self.text[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self, at: usize) -> usize {
        let mut boundaries = self.text[at..].grapheme_indices(true);
        boundaries.next();
        boundaries.next().map_or(self.text.len(), |(i, _)| at + i)
    }

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let insertion = if self.layout.rows > 1 {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n', '\t'], " ")
        };
        let end = insertion.floor_char_boundary(
            insertion.len().min(
                self.layout
                    .limit
                    .saturating_sub(self.text.len() - range.len()),
            ),
        );
        self.caret = range.start + end;
        self.anchor = self.caret;
        self.text.replace_range(range, &insertion[..end]);
        self.composition = None;
        cx.emit(QueryChanged(self.text.clone()));
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.composing() {
            return;
        }
        let mods = event.keystroke.modifiers;
        let selection = self.selection();
        let key = event.keystroke.key.as_str();
        match key {
            "a" if mods.control => {
                self.anchor = 0;
                self.set_caret(self.text.len(), true, cx);
            }
            "c" | "x" if mods.control => {
                if !selection.is_empty() {
                    let _ = self
                        .intents
                        .try_send(Intent::CopyInput(self.text[selection.clone()].to_owned()));
                    if key == "x" {
                        self.replace(selection, "", cx);
                    }
                }
            }
            "v" if mods.control => {
                let _ = self
                    .intents
                    .try_send(Intent::PasteInput(self.focus.clone()));
            }
            "left" if !mods.alt => {
                let p = if !mods.shift && !selection.is_empty() {
                    selection.start
                } else {
                    self.previous(self.caret)
                };
                self.set_caret(p, mods.shift, cx);
            }
            "right" if !mods.alt => {
                let p = if !mods.shift && !selection.is_empty() {
                    selection.end
                } else {
                    self.next(self.caret)
                };
                self.set_caret(p, mods.shift, cx);
            }
            "home" if mods.control => self.set_caret(0, mods.shift, cx),
            "end" if mods.control => self.set_caret(self.text.len(), mods.shift, cx),
            "home" => self.set_caret(self.line_range().start, mods.shift, cx),
            "end" => self.set_caret(self.line_range().end, mods.shift, cx),
            "up" | "down" if self.layout.rows > 1 && !mods.control && !mods.alt => {
                self.set_caret(self.adjacent_line(key == "up"), mods.shift, cx);
            }
            "enter" if self.layout.rows > 1 && !mods.control && !mods.alt && !mods.platform => {
                self.replace(selection, "\n", cx);
            }
            "backspace" | "delete" => {
                let range = if !selection.is_empty() {
                    selection
                } else if key == "backspace" {
                    self.previous(self.caret)..self.caret
                } else {
                    self.caret..self.next(self.caret)
                };
                if !range.is_empty() {
                    self.replace(range, "", cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn mouse_index(&self, point: Point<Pixels>) -> Option<usize> {
        let geometry = self.geometry.as_ref()?;
        let row = ((point.y - geometry.bounds.top()) / geometry.line_height) as usize;
        let line = &geometry.lines[row.min(geometry.lines.len() - 1)];
        let x = point.x - geometry.bounds.left() + line.offset;
        let index = line.range.start + line.line.closest_index_for_x(x).min(line.range.len());
        Some(self.text.floor_char_boundary(index.min(self.text.len())))
    }

    fn line_range(&self) -> Range<usize> {
        let start = self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[self.caret..]
            .find('\n')
            .map_or(self.text.len(), |i| self.caret + i);
        start..end
    }

    fn adjacent_line(&self, previous: bool) -> usize {
        let line = self.line_range();
        let target = if previous {
            if line.start == 0 {
                return self.caret;
            }
            let end = line.start - 1;
            let start = self.text[..end].rfind('\n').map_or(0, |i| i + 1);
            start..end
        } else {
            if line.end == self.text.len() {
                return self.caret;
            }
            let start = line.end + 1;
            let end = self.text[start..]
                .find('\n')
                .map_or(self.text.len(), |i| start + i);
            start..end
        };
        let column = self.text[line.start..self.caret].graphemes(true).count();
        self.text[target.clone()]
            .grapheme_indices(true)
            .nth(column)
            .map_or(target.end, |(index, _)| target.start + index)
    }

    fn shape_visible_lines(&self, bounds: Bounds<Pixels>, window: &mut Window) -> Geometry {
        let active = self.text.as_bytes()[..self.caret]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        let top = (active + 1).saturating_sub(self.layout.rows);
        let mut start = 0;
        let ranges = self
            .text
            .split('\n')
            .map(|line| {
                let range = start..start + line.len();
                start = range.end + 1;
                range
            });
        let empty = self.text.is_empty();
        let lines = ranges
            .skip(top)
            .take(self.layout.rows)
            .map(|range| {
                let raw: SharedString = if empty {
                    self.placeholder.clone()
                } else {
                    self.text[range.clone()].replace('\t', " ").into()
                };
                let run = TextRun {
                    len: raw.len(),
                    font: font("Segoe UI"),
                    color: if empty { self.muted } else { self.foreground },
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = if let Some(mark) = &self.composition {
                    let start = mark.range.start.clamp(range.start, range.end) - range.start;
                    let end = mark.range.end.clamp(range.start, range.end) - range.start;
                    let runs = [
                        TextRun {
                            len: start,
                            ..run.clone()
                        },
                        TextRun {
                            len: end - start,
                            underline: Some(UnderlineStyle {
                                color: Some(self.accent),
                                thickness: px(1.),
                                wavy: false,
                            }),
                            ..run.clone()
                        },
                        TextRun {
                            len: raw.len() - end,
                            ..run
                        },
                    ];
                    let runs: Vec<_> = runs.into_iter().filter(|run| run.len > 0).collect();
                    window
                        .text_system()
                        .shape_line(raw, px(self.layout.font_size), &runs, None)
                } else {
                    window
                        .text_system()
                        .shape_line(raw, px(self.layout.font_size), &[run], None)
                };
                let offset = if (range.start..=range.end).contains(&self.caret) {
                    (line.x_for_index(self.caret - range.start) - bounds.size.width + px(3.))
                        .max(px(0.))
                } else {
                    px(0.)
                };
                VisibleLine {
                    line,
                    range,
                    offset,
                }
            })
            .collect();
        Geometry {
            lines,
            bounds,
            line_height: px(self.layout.line_height),
        }
    }
}

fn from_utf16(text: &str, offset: usize) -> usize {
    let mut count = 0;
    for (i, ch) in text.char_indices() {
        count += ch.len_utf16();
        if count > offset {
            return i;
        }
    }
    text.len()
}

fn to_utf16(text: &str, offset: usize) -> usize {
    text[..offset].encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::{from_utf16, to_utf16};

    #[test]
    fn ime_offsets_round_trip_for_chinese_and_surrogate_pairs() {
        let text = "中文😀A";
        for (utf16, bytes) in [(0, 0), (1, 3), (2, 6), (4, 10), (5, 11)] {
            assert_eq!(from_utf16(text, utf16), bytes);
            assert_eq!(to_utf16(text, bytes), utf16);
        }
        assert_eq!(from_utf16(text, 3), 6);
        assert_eq!(from_utf16(text, 99), text.len());
    }
}

impl EntityInputHandler for SearchInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let limit = from_utf16(&self.text, range.start)..from_utf16(&self.text, range.end);
        let text = self.text.get(limit.clone())?;
        *adjusted = Some(to_utf16(&self.text, limit.start)..to_utf16(&self.text, limit.end));
        Some(text.to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let range = self.selection();
        Some(UTF16Selection {
            range: to_utf16(&self.text, range.start)..to_utf16(&self.text, range.end),
            reversed: self.caret < self.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let range = &self.composition.as_ref()?.range;
        Some(to_utf16(&self.text, range.start)..to_utf16(&self.text, range.end))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.composition.take().is_some() {
            cx.emit(QueryChanged(self.text.clone()));
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if range.is_none() && text.is_empty() && self.composing() {
            self.cancel_composition(cx);
            return;
        }
        let range = range
            .map(|range| from_utf16(&self.text, range.start)..from_utf16(&self.text, range.end))
            .or_else(|| self.composition.as_ref().map(|marked| marked.range.clone()))
            .unwrap_or_else(|| self.selection());
        self.replace(range, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            self.cancel_composition(cx);
            return;
        }
        let range = range
            .map(|range| from_utf16(&self.text, range.start)..from_utf16(&self.text, range.end))
            .or_else(|| self.composition.as_ref().map(|marked| marked.range.clone()))
            .unwrap_or_else(|| self.selection());
        let original = self
            .composition
            .take()
            .map_or_else(|| self.text[range.clone()].to_owned(), |old| old.original);
        let start = range.start;
        self.text.replace_range(range, text);
        let end = start + text.len();
        self.composition = Some(Composition {
            range: start..end,
            original,
        });
        (self.anchor, self.caret) = selection.map_or((end, end), |selection| {
            (
                start + from_utf16(text, selection.start),
                start + from_utf16(text, selection.end),
            )
        });
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let geometry = self.geometry.as_ref()?;
        let start = from_utf16(&self.text, range.start);
        let end = from_utf16(&self.text, range.end);
        let (row, line) = geometry
            .lines
            .iter()
            .enumerate()
            .find(|(_, line)| start >= line.range.start && start <= line.range.end)?;
        let at = |index: usize| {
            geometry.bounds.left() - line.offset
                + line
                    .line
                    .x_for_index(index.min(line.range.end) - line.range.start)
        };
        let first = at(start);
        let last = at(end);
        Some(Bounds::new(
            point(
                first.min(last),
                geometry.bounds.top() + geometry.line_height * row as f32,
            ),
            size((last - first).abs().max(px(1.)), geometry.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(to_utf16(&self.text, self.mouse_index(point)?))
    }
}

impl Render for SearchInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        div()
            .when(self.layout.grow, |input| input.flex_1())
            .when(!self.layout.grow, |input| input.w_full().flex_shrink_0())
            .min_w_0()
            .h(px(self.layout.line_height * self.layout.rows as f32))
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .overflow_hidden()
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|input, event: &MouseDownEvent, window, cx| {
                    input.focus.focus(window);
                    if !input.composing()
                        && let Some(index) = input.mouse_index(event.position)
                    {
                        input.dragging = true;
                        input.set_caret(index, event.modifiers.shift, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|input, _, _, _| input.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|input, _, _, _| input.dragging = false),
            )
            .on_mouse_move(cx.listener(|input, event: &MouseMoveEvent, _, cx| {
                if input.dragging
                    && let Some(index) = input.mouse_index(event.position)
                {
                    input.set_caret(index, true, cx);
                }
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        let input = view.read(cx);
                        let focus = input.focus.clone();
                        let geometry = input.shape_visible_lines(bounds, window);
                        let selection = input.selection();
                        let accent = input.accent;
                        let caret = input.caret;
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, view.clone()),
                            cx,
                        );
                        let show_caret = focus.is_focused(window)
                            && selection.is_empty()
                            && window.is_window_active();
                        let height = geometry.line_height;
                        for (row, visible) in geometry.lines.iter().enumerate() {
                            let origin = point(
                                bounds.left() - visible.offset,
                                bounds.top() + height * row as f32,
                            );
                            let range = &visible.range;
                            let line = &visible.line;
                            let start = selection.start.max(range.start);
                            let end = selection.end.min(range.end);
                            if start < end {
                                let a = line.x_for_index(start - range.start);
                                let b = line.x_for_index(end - range.start);
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(origin.x + a.min(b), origin.y + px(3.)),
                                        size((b - a).abs(), height - px(6.)),
                                    ),
                                    accent.opacity(0.25),
                                ));
                            }
                            let _ = line.paint(origin, height, window, cx);
                            if show_caret && (range.start..=range.end).contains(&caret) {
                                let x = line.x_for_index(caret - range.start);
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(origin.x + x, origin.y + px(5.)),
                                        size(px(1.5), height - px(10.)),
                                    ),
                                    accent,
                                ));
                            }
                        }
                        view.update(cx, |input, _| {
                            input.geometry = Some(geometry);
                        });
                    },
                )
                .size_full(),
            )
    }
}

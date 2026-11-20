use std::ops::Range;

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
}

struct Composition {
    range: Range<usize>,
    original: String,
}

struct Geometry {
    line: ShapedLine,
    bounds: Bounds<Pixels>,
    offset: Pixels,
    text_is_empty: bool,
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
        }
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
        let single_line = text.replace(['\r', '\n', '\t'], " ");
        let end = single_line.floor_char_boundary(
            single_line
                .len()
                .min(8192 - (self.text.len() - range.len()).min(8192)),
        );
        self.caret = range.start + end;
        self.anchor = self.caret;
        self.text.replace_range(range, &single_line[..end]);
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
                let _ = self.intents.try_send(Intent::PasteInput);
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
            "home" => self.set_caret(0, mods.shift, cx),
            "end" => self.set_caret(self.text.len(), mods.shift, cx),
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
        if geometry.text_is_empty {
            return Some(0);
        }
        let x = point.x - geometry.bounds.left() + geometry.offset;
        Some(geometry.line.closest_index_for_x(x).min(self.text.len()))
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
        let at = |i| {
            geometry.bounds.left() - geometry.offset
                + geometry.line.x_for_index(from_utf16(&self.text, i))
        };
        let first = at(range.start);
        let last = at(range.end);
        Some(Bounds::new(
            point(first.min(last), geometry.bounds.top()),
            size(
                (last - first).abs().max(px(1.)),
                geometry.bounds.size.height,
            ),
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
            .flex_1()
            .min_w_0()
            .h(px(34.))
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
                        let empty = input.text.is_empty();
                        let raw = if empty {
                            input.placeholder.clone()
                        } else {
                            input.text.clone().into()
                        };
                        let run = TextRun {
                            len: raw.len(),
                            font: font("Segoe UI"),
                            color: if empty { input.muted } else { input.foreground },
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let runs = if let Some(mark) = &input.composition {
                            [
                                TextRun {
                                    len: mark.range.start,
                                    ..run.clone()
                                },
                                TextRun {
                                    len: mark.range.len(),
                                    underline: Some(UnderlineStyle {
                                        color: Some(input.accent),
                                        thickness: px(1.),
                                        wavy: false,
                                    }),
                                    ..run.clone()
                                },
                                TextRun {
                                    len: raw.len() - mark.range.end,
                                    ..run
                                },
                            ]
                            .into_iter()
                            .filter(|run| run.len > 0)
                            .collect::<Vec<_>>()
                        } else {
                            vec![run]
                        };
                        let line = window.text_system().shape_line(raw, px(22.), &runs, None);
                        let x = line.x_for_index(input.caret);
                        let offset = (x - bounds.size.width + px(3.)).max(px(0.));
                        let origin = point(bounds.left() - offset, bounds.top());
                        let selection = input.selection();
                        let accent = input.accent;
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, view.clone()),
                            cx,
                        );
                        if !selection.is_empty() {
                            let a = line.x_for_index(selection.start);
                            let b = line.x_for_index(selection.end);
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(origin.x + a.min(b), origin.y + px(3.)),
                                    size((b - a).abs(), px(28.)),
                                ),
                                accent.opacity(0.25),
                            ));
                        }
                        let _ = line.paint(origin, bounds.size.height, window, cx);
                        if focus.is_focused(window)
                            && selection.is_empty()
                            && window.is_window_active()
                        {
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(origin.x + x, origin.y + px(5.)),
                                    size(px(1.5), px(24.)),
                                ),
                                accent,
                            ));
                        }
                        view.update(cx, |input, _| {
                            input.geometry = Some(Geometry {
                                line,
                                bounds,
                                offset,
                                text_is_empty: empty,
                            })
                        });
                    },
                )
                .size_full(),
            )
    }
}

//! One table for all three tabs: columns, and rows of pre-formatted cells.
//!
//! The rows are built from the portfolio each time it changes; the table only
//! draws them, virtualised, so an account with hundreds of positions scrolls
//! as easily as one with four.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::{Disableable, Icon, IconName, Sizable, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, ElementId, Hsla, SharedString, Window, div, px};

use crate::theme::{MONO, Palette};

/// How a piece of a cell is coloured.
#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Text,
    Muted,
    Gain,
    Loss,
    /// A small tag: a builder dex's name, a wallet, an order flag.
    Tag,
}

impl Tone {
    /// The tone of a signed amount.
    pub fn signed(sign: i32) -> Tone {
        match sign {
            s if s > 0 => Tone::Gain,
            s if s < 0 => Tone::Loss,
            _ => Tone::Text,
        }
    }

    fn color(self) -> Hsla {
        match self {
            Tone::Text => Palette::text(),
            Tone::Muted | Tone::Tag => Palette::muted(),
            Tone::Gain => Palette::gain(),
            Tone::Loss => Palette::loss(),
        }
    }
}

/// What an icon button in a row shows.
#[derive(Clone, Copy, PartialEq)]
pub enum Glyph {
    Pencil,
    Cross,
    /// A word in place of an icon: `Limit`, `Market`.
    Word(&'static str),
}

/// A button in a row. An icon has no text, so `name` is what it is called:
/// shown on hover and read by assistive technology. A word button shows its
/// word and is still called by `name`.
#[derive(Clone)]
pub struct RowButton {
    /// What the button does; with `key` it identifies the press.
    pub id: &'static str,
    /// Which row's thing it acts on.
    pub key: u64,
    pub glyph: Glyph,
    pub name: SharedString,
    pub disabled: bool,
    /// Its action is under way: it shows a spinner in place of its glyph.
    pub busy: bool,
}

/// What a press of a row's button is reported to.
pub type OnPress = Rc<dyn Fn(&'static str, u64, &mut Window, &mut App)>;

/// One cell: one or more pieces of text, each with its own tone, or a row's
/// buttons.
#[derive(Clone, Default)]
pub struct Cell {
    parts: Vec<(SharedString, Tone)>,
    buttons: Vec<RowButton>,
}

impl Cell {
    pub fn new(text: impl Into<SharedString>, tone: Tone) -> Self {
        Self {
            parts: vec![(text.into(), tone)],
            buttons: Vec::new(),
        }
    }

    /// A cell of icon buttons.
    pub fn buttons(buttons: Vec<RowButton>) -> Self {
        Self {
            parts: Vec::new(),
            buttons,
        }
    }

    pub fn text(text: impl Into<SharedString>) -> Self {
        Self::new(text, Tone::Text)
    }

    pub fn muted(text: impl Into<SharedString>) -> Self {
        Self::new(text, Tone::Muted)
    }

    /// Add another piece after the first.
    pub fn and(mut self, text: impl Into<SharedString>, tone: Tone) -> Self {
        self.parts.push((text.into(), tone));
        self
    }

    /// Buttons after the cell's text: a value with its edit control.
    pub fn with_buttons(mut self, buttons: Vec<RowButton>) -> Self {
        self.buttons = buttons;
        self
    }

    /// The cell's buttons, for tests.
    #[cfg(test)]
    pub fn row_buttons(&self) -> &[RowButton] {
        &self.buttons
    }

    fn plain(&self) -> String {
        self.parts
            .iter()
            .map(|(t, _)| t.as_ref())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A column: its heading, width, and whether it holds numbers.
#[derive(Clone)]
pub struct Col {
    pub name: &'static str,
    pub width: f32,
    /// Numbers are right-aligned and monospaced so digits line up.
    pub numeric: bool,
    /// Kept at the right edge while the rest scrolls sideways: a row's
    /// controls stay in view however narrow the window.
    pub pinned: bool,
}

impl Col {
    pub const fn text(name: &'static str, width: f32) -> Self {
        Self {
            name,
            width,
            numeric: false,
            pinned: false,
        }
    }

    pub const fn number(name: &'static str, width: f32) -> Self {
        Self {
            name,
            width,
            numeric: true,
            pinned: false,
        }
    }

    /// Keep this column at the right edge. Only the last columns may be.
    pub const fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }
}

pub struct Grid {
    cols: Vec<Col>,
    rows: Vec<Vec<Cell>>,
    /// What to say when there are no rows.
    empty: SharedString,
    on_press: Option<OnPress>,
}

impl Grid {
    pub fn new(cols: Vec<Col>, empty: impl Into<SharedString>) -> Self {
        Self {
            cols,
            rows: Vec::new(),
            empty: empty.into(),
            on_press: None,
        }
    }

    /// Report presses of row buttons to `on_press`.
    pub fn on_press(mut self, on_press: OnPress) -> Self {
        self.on_press = Some(on_press);
        self
    }

    pub fn set_rows(&mut self, rows: Vec<Vec<Cell>>) {
        self.rows = rows;
    }
}

impl TableDelegate for Grid {
    fn columns_count(&self, _: &App) -> usize {
        self.cols.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let col = &self.cols[col_ix];
        let mut column = Column::new(col.name, col.name)
            .width(px(col.width))
            .movable(false)
            .selectable(false);
        if col.pinned {
            column = column.fixed_right();
        }
        if col.numeric {
            column.text_right()
        } else {
            column
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let numeric = self.cols[col_ix].numeric;
        let Some(cell) = self.rows.get(row_ix).and_then(|r| r.get(col_ix)) else {
            return div().into_any_element();
        };
        // A spinner turns; with reduced motion the glyph stays and the
        // button's name says what is happening.
        let still = cx.reduce_motion();
        let buttons = cell.buttons.iter().map(|b| {
            let on_press = self.on_press.clone();
            let (id, key) = (b.id, b.key);
            let button = Button::new(ElementId::NamedInteger(b.id.into(), b.key)).small();
            let button = match b.glyph {
                Glyph::Pencil => button.ghost().icon(crate::ui::pencil_icon()),
                Glyph::Cross => button.ghost().icon(Icon::new(IconName::Close)),
                // A word on its own reads as text: the outline says it can
                // be pressed.
                Glyph::Word(word) => button.outline().label(word),
            };
            button
                .tooltip(b.name.clone())
                .accessibility_label(b.name.clone())
                .disabled(b.disabled)
                .loading(b.busy && !still)
                .on_click(move |_, window, cx| {
                    if let Some(on_press) = &on_press {
                        on_press(id, key, window, cx);
                    }
                })
        });
        let buttons: Vec<_> = buttons.collect();
        if cell.parts.is_empty() {
            return h_flex()
                .w_full()
                .gap_1()
                .justify_end()
                .children(buttons)
                .into_any_element();
        }
        h_flex()
            .w_full()
            .gap_2()
            .when(numeric, |d| d.justify_end())
            .font_family(MONO)
            .text_size(px(12.5))
            .whitespace_nowrap()
            .children(cell.parts.iter().map(|(text, tone)| {
                let piece = div().text_color(tone.color()).child(text.clone());
                if *tone == Tone::Tag {
                    piece
                        .px(px(5.))
                        .rounded(px(4.))
                        .bg(Palette::raise())
                        .text_size(px(11.))
                } else {
                    piece
                }
            }))
            .children(buttons)
            .into_any_element()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        self.rows
            .get(row_ix)
            .and_then(|r| r.get(col_ix))
            // A cell of buttons only has no words of its own: its buttons
            // are named.
            .filter(|c| !c.parts.is_empty())
            .map(|c| c.plain().into())
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_color(Palette::faint())
            .child(self.empty.clone())
    }
}

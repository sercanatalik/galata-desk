//! One table for all three tabs: columns, and rows of pre-formatted cells.
//!
//! The rows are built from the portfolio each time it changes; the table only
//! draws them, virtualised, so an account with hundreds of positions scrolls
//! as easily as one with four.
//!
//! A column may be sortable: its header then carries the table's sort
//! control, which cycles largest first, smallest first, and back to the
//! order the rows were given in. Each row carries a key per column for this,
//! so sorting is by the figure, not by its formatted text. The sort is kept
//! when the rows are replaced.

use std::cmp::Ordering;
use std::rc::Rc;

use galata_desk_core::rust_decimal::Decimal;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
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

    pub(crate) fn color(self) -> Hsla {
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
    Copy,
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
    /// Shown on hover: what the cell's words stand for.
    tip: Option<SharedString>,
}

impl Cell {
    pub fn new(text: impl Into<SharedString>, tone: Tone) -> Self {
        Self {
            parts: vec![(text.into(), tone)],
            buttons: Vec::new(),
            tip: None,
        }
    }

    /// A cell of icon buttons.
    pub fn buttons(buttons: Vec<RowButton>) -> Self {
        Self {
            parts: Vec::new(),
            buttons,
            tip: None,
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

    /// Explain the cell on hover.
    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }

    /// The cell's tooltip, for tests.
    #[cfg(test)]
    pub fn tip_text(&self) -> Option<&str> {
        self.tip.as_deref()
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

    pub fn plain(&self) -> String {
        self.parts
            .iter()
            .map(|(t, _)| t.as_ref())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What a row is sorted by in one column.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Num(Decimal),
    /// Compared without regard to case.
    Text(String),
    /// No value, as a liquidation price the venue does not give: always
    /// listed last, whichever way the column is sorted.
    Missing,
}

impl Key {
    pub fn num(value: Option<Decimal>) -> Key {
        value.map_or(Key::Missing, Key::Num)
    }

    fn cmp(&self, other: &Key) -> Ordering {
        match (self, other) {
            (Key::Num(a), Key::Num(b)) => a.cmp(b),
            (Key::Text(a), Key::Text(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
            _ => Ordering::Equal,
        }
    }
}

/// How two rows' keys order under a sort; missing values go last.
fn compare(a: Option<&Key>, b: Option<&Key>, sort: ColumnSort) -> Ordering {
    let present = |k: Option<&Key>| k.filter(|k| **k != Key::Missing).cloned();
    match (present(a), present(b)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => match sort {
            ColumnSort::Descending => a.cmp(&b).reverse(),
            _ => a.cmp(&b),
        },
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
    /// Its header offers to sort the rows by it.
    pub sortable: bool,
}

impl Col {
    pub const fn text(name: &'static str, width: f32) -> Self {
        Self {
            name,
            width,
            numeric: false,
            pinned: false,
            sortable: false,
        }
    }

    pub const fn number(name: &'static str, width: f32) -> Self {
        Self {
            name,
            width,
            numeric: true,
            pinned: false,
            sortable: false,
        }
    }

    /// Keep this column at the right edge. Only the last columns may be.
    pub const fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    /// Let the rows be sorted by this column.
    pub const fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }
}

pub struct Grid {
    cols: Vec<Col>,
    rows: Vec<Vec<Cell>>,
    /// Each row's sort keys, one per column; empty when it has none.
    keys: Vec<Vec<Key>>,
    /// The column sorted by and which way; `None` keeps the given order.
    sort: Option<(usize, ColumnSort)>,
    /// The rows in the order they are drawn: indices into `rows`.
    order: Vec<usize>,
    /// What to say when there are no rows.
    empty: SharedString,
    on_press: Option<OnPress>,
}

impl Grid {
    pub fn new(cols: Vec<Col>, empty: impl Into<SharedString>) -> Self {
        Self {
            cols,
            rows: Vec::new(),
            keys: Vec::new(),
            sort: None,
            order: Vec::new(),
            empty: empty.into(),
            on_press: None,
        }
    }

    /// What to say when there are no rows, from now on.
    pub fn set_empty(&mut self, empty: impl Into<SharedString>) {
        self.empty = empty.into();
    }

    /// What the table says when it has no rows, for tests.
    #[cfg(test)]
    pub fn empty_text(&self) -> String {
        self.empty.to_string()
    }

    /// A cell as given, for tests: by its row as drawn and its column.
    #[cfg(test)]
    pub fn cell(&self, row_ix: usize, col_ix: usize) -> Option<&Cell> {
        self.row(row_ix).and_then(|r| r.get(col_ix))
    }

    /// A copy with the same columns, rows and button handler, for drawing
    /// the same list in a second place. Its sort is its own.
    pub fn mirror(&self) -> Grid {
        let mut grid = Grid {
            cols: self.cols.clone(),
            rows: Vec::new(),
            keys: Vec::new(),
            sort: None,
            order: Vec::new(),
            empty: self.empty.clone(),
            on_press: self.on_press.clone(),
        };
        grid.copy_rows_from(self);
        grid
    }

    /// Take `other`'s rows and what it says when empty, keeping this grid's
    /// own sort.
    pub fn copy_rows_from(&mut self, other: &Grid) {
        self.empty = other.empty.clone();
        self.set_keyed_rows(other.rows.clone(), other.keys.clone());
    }

    /// Press a row button, as a click on it does, for tests.
    #[cfg(test)]
    pub fn press(&self, id: &'static str, key: u64, window: &mut Window, cx: &mut App) {
        if let Some(on_press) = &self.on_press {
            on_press(id, key, window, cx);
        }
    }

    /// How many columns the list has, for tests.
    #[cfg(test)]
    pub fn col_count(&self) -> usize {
        self.cols.len()
    }

    /// Report presses of row buttons to `on_press`.
    pub fn on_press(mut self, on_press: OnPress) -> Self {
        self.on_press = Some(on_press);
        self
    }

    pub fn set_rows(&mut self, rows: Vec<Vec<Cell>>) {
        self.set_keyed_rows(rows, Vec::new());
    }

    /// Rows with their sort keys, in their default order. The current sort,
    /// if any, is applied to them.
    pub fn set_keyed_rows(&mut self, rows: Vec<Vec<Cell>>, keys: Vec<Vec<Key>>) {
        self.rows = rows;
        self.keys = keys;
        self.reorder();
    }

    fn reorder(&mut self) {
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        if let Some((col, sort)) = self.sort.filter(|(_, s)| *s != ColumnSort::Default) {
            let key = |ix: usize| self.keys.get(ix).and_then(|k| k.get(col));
            // Stable: rows that tie keep their default order.
            order.sort_by(|&a, &b| compare(key(a), key(b), sort));
        }
        self.order = order;
    }

    fn row(&self, row_ix: usize) -> Option<&Vec<Cell>> {
        self.order.get(row_ix).and_then(|&ix| self.rows.get(ix))
    }

    /// A column's cells as drawn, top to bottom, for tests.
    #[cfg(test)]
    pub fn column_texts(&self, col_ix: usize) -> Vec<String> {
        (0..self.order.len())
            .filter_map(|ix| self.row(ix).and_then(|r| r.get(col_ix)))
            .map(Cell::plain)
            .collect()
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
        if col.sortable {
            // The header's control shows the sort the rows are in.
            column = column.sort(match self.sort {
                Some((ix, sort)) if ix == col_ix => sort,
                _ => ColumnSort::Default,
            });
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
        let Some(cell) = self.row(row_ix).and_then(|r| r.get(col_ix)) else {
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
                Glyph::Copy => button.ghost().icon(Icon::new(IconName::Copy)),
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
        let tip = cell.tip.clone();
        h_flex()
            .id(ElementId::NamedInteger(
                "cell".into(),
                (row_ix * 64 + col_ix) as u64,
            ))
            .w_full()
            .gap_2()
            .when(numeric, |d| d.justify_end())
            .when_some(tip, |d, tip| {
                d.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
            })
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
        self.row(row_ix)
            .and_then(|r| r.get(col_ix))
            // A cell of buttons only has no words of its own: its buttons
            // are named.
            .filter(|c| !c.parts.is_empty())
            .map(|c| c.plain().into())
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.sort = Some((col_ix, sort));
        self.reorder();
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

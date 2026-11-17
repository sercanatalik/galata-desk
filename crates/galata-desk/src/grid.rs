//! One table for all three tabs: columns, and rows of pre-formatted cells.
//!
//! The rows are built from the portfolio each time it changes; the table only
//! draws them, virtualised, so an account with hundreds of positions scrolls
//! as easily as one with four.

use gpui_kit::component::h_flex;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Hsla, SharedString, Window, div, px};

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

/// One cell: one or more pieces of text, each with its own tone.
#[derive(Clone, Default)]
pub struct Cell {
    parts: Vec<(SharedString, Tone)>,
}

impl Cell {
    pub fn new(text: impl Into<SharedString>, tone: Tone) -> Self {
        Self {
            parts: vec![(text.into(), tone)],
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
}

impl Col {
    pub const fn text(name: &'static str, width: f32) -> Self {
        Self {
            name,
            width,
            numeric: false,
        }
    }

    pub const fn number(name: &'static str, width: f32) -> Self {
        Self {
            name,
            width,
            numeric: true,
        }
    }
}

pub struct Grid {
    cols: Vec<Col>,
    rows: Vec<Vec<Cell>>,
    /// What to say when there are no rows.
    empty: SharedString,
}

impl Grid {
    pub fn new(cols: Vec<Col>, empty: impl Into<SharedString>) -> Self {
        Self {
            cols,
            rows: Vec::new(),
            empty: empty.into(),
        }
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
        let column = Column::new(col.name, col.name)
            .width(px(col.width))
            .movable(false)
            .selectable(false);
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
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let numeric = self.cols[col_ix].numeric;
        let Some(cell) = self.rows.get(row_ix).and_then(|r| r.get(col_ix)) else {
            return div().into_any_element();
        };
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
            .into_any_element()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        self.rows
            .get(row_ix)
            .and_then(|r| r.get(col_ix))
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

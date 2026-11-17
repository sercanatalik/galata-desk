//! The Galata house look: the palette galata-tower's screen uses, as a GPUI
//! Kit theme, plus the few colours a trading screen needs that a general
//! theme has no name for.
//!
//! Gains are blue and losses are orange, as everywhere in the family: the pair
//! survives the common colour-vision deficiencies that red and green do not.

use std::borrow::Cow;
use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig};
use gpui_kit::{App, Hsla, Rgba, rgb};

const THEME_JSON: &str = include_str!("../../../assets/themes/galata-dark.json");

/// The face every number is set in, so digits line up down a column.
pub const MONO: &str = "JetBrains Mono";

macro_rules! font {
    ($name:literal) => {
        Cow::Borrowed(include_bytes!(concat!("../../../assets/fonts/", $name)).as_slice())
    };
}

/// Where the app's own images live among the assets.
pub const LOGO: &str = "galata-desk/logo.svg";

/// The app's assets: the component kit's icons, and the Galata Tower mark.
///
/// The mark is the family's own, the same shape every sibling ships as
/// `assets/logo.svg`. It is drawn as a mask, so it takes whatever colour the
/// element showing it has.
pub struct Assets;

impl gpui_kit::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../../../assets/logo-on-dark.svg"
            ))));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<gpui_kit::SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

/// Colours the theme has no slot for. Values are galata-tower's tokens.
pub struct Palette;

impl Palette {
    /// The window's ground.
    pub fn ground() -> Hsla {
        hex(0x0e1012)
    }
    /// A panel resting on the ground.
    pub fn panel() -> Hsla {
        hex(0x15181b)
    }
    /// A surface raised off a panel: hover, placeholders.
    pub fn raise() -> Hsla {
        hex(0x1b1f23)
    }
    /// A panel's outline.
    pub fn line() -> Hsla {
        hex(0x262b30)
    }
    /// A rule between rows.
    pub fn rule() -> Hsla {
        hex(0x20252a)
    }
    /// Body text.
    pub fn text() -> Hsla {
        hex(0xece8df)
    }
    /// Labels.
    pub fn muted() -> Hsla {
        hex(0xa29e94)
    }
    /// The quietest text: units, axis ticks.
    pub fn faint() -> Hsla {
        hex(0x6e6b64)
    }
    /// The accent.
    pub fn amber() -> Hsla {
        hex(0xf0a73a)
    }
    /// The ground under an amber notice.
    pub fn amber_ground() -> Hsla {
        hex(0x2a2112)
    }
    /// The outline of an amber notice.
    pub fn amber_line() -> Hsla {
        hex(0x5c4520)
    }
    /// Text on an amber notice.
    pub fn amber_text() -> Hsla {
        hex(0xf6d9a8)
    }
    /// A gain, a buy, a long.
    pub fn gain() -> Hsla {
        hex(0x6aa9e0)
    }
    /// A loss, a sell, a short.
    pub fn loss() -> Hsla {
        hex(0xe8843f)
    }
}

fn hex(value: u32) -> Hsla {
    let rgba: Rgba = rgb(value);
    rgba.into()
}

/// Load the embedded faces and install the theme. Call once, after
/// `gpui_kit::init`.
pub fn install(cx: &mut App) {
    cx.text_system()
        .add_fonts(vec![
            font!("InstrumentSans-Regular.ttf"),
            font!("InstrumentSans-Medium.ttf"),
            font!("InstrumentSans-SemiBold.ttf"),
            font!("JetBrainsMono-Regular.ttf"),
            font!("JetBrainsMono-Medium.ttf"),
        ])
        .expect("the embedded fonts are valid font files");

    let config: ThemeConfig =
        serde_json::from_str(THEME_JSON).expect("the embedded theme is a valid theme file");
    let config = Rc::new(config);
    Theme::update(cx, |theme| theme.apply_config(&config));
}

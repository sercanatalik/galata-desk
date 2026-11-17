//! `GALATA_DESK_SHOT=<file.png>`: render the window offscreen, write it, quit.
//!
//! `GALATA_DESK_SHOT_AFTER_MS` sets how long to let the screen settle first
//! (default 2500), for screens that wait on the venue.

use std::time::Duration;

use gpui_kit::{AnyWindowHandle, App, AppContext as _};

pub fn arm(window: AnyWindowHandle, cx: &mut App) {
    let Ok(path) = std::env::var("GALATA_DESK_SHOT") else {
        return;
    };
    let wait = std::env::var("GALATA_DESK_SHOT_AFTER_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2500);
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(wait))
            .await;
        // A hidden window is not redrawn on its own. Draw the current state,
        // give the chart's draw-in animation time to finish, then draw again
        // and capture, so the image is of this moment and fully drawn.
        let _ = cx.update_window(window, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        cx.background_executor()
            .timer(Duration::from_millis(1300))
            .await;
        let result = cx.update_window(window, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
            window.render_to_image()
        });
        match result {
            Ok(Ok(image)) => match image.save(&path) {
                Ok(()) => eprintln!("shot: wrote {path}"),
                Err(e) => eprintln!("shot: could not write {path}: {e}"),
            },
            Ok(Err(e)) => eprintln!("shot: could not render: {e}"),
            Err(e) => eprintln!("shot: window is gone: {e}"),
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

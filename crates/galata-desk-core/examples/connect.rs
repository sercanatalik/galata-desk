//! Run one connection attempt from the terminal.
//!
//! ```text
//! cargo run -p galata-desk-core --example connect
//! ```
//!
//! Prints the address to open in a browser, then what happens.

use galata_desk_core::connect::{Attempt, AttemptEvent};
use galata_desk_core::store::Store;

#[tokio::main]
async fn main() {
    let project_id = Store::default_location().and_then(|s| s.project_id());
    let mut attempt = Attempt::start(project_id).await.expect("the bridge starts");
    println!("open {}", attempt.url());
    while let Some(event) = attempt.next_event().await {
        match event {
            AttemptEvent::Connected(address) => {
                println!("connected {address}");
                break;
            }
            AttemptEvent::Rejected(why) => println!("rejected: {why}"),
            AttemptEvent::Expired => {
                println!("expired");
                break;
            }
        }
    }
}

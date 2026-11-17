//! Try the macOS Keychain: store, read back and delete an entry under a
//! throwaway service name. Nothing of the app's own is touched.
//!
//! ```text
//! cargo run -p galata-desk-core --example keychain
//! ```

#[cfg(target_os = "macos")]
fn main() {
    use galata_desk_core::secrets::{Keychain, Secrets};

    let keychain = Keychain::under("dev.galata.desk.example");
    let name = "throwaway";
    assert_eq!(
        keychain.get(name).unwrap(),
        None,
        "nothing there to begin with"
    );
    keychain.set(name, "not-a-real-secret").unwrap();
    assert_eq!(
        keychain.get(name).unwrap().as_deref(),
        Some("not-a-real-secret")
    );
    keychain.set(name, "replaced").unwrap();
    assert_eq!(keychain.get(name).unwrap().as_deref(), Some("replaced"));
    keychain.delete(name).unwrap();
    assert_eq!(keychain.get(name).unwrap(), None);
    keychain.delete(name).unwrap();
    println!("keychain: stored, read back, replaced and deleted an entry");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("the keychain store is macOS only");
}

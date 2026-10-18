// The exact pinned bread-utils version starts automatic reporting in connect().
// Keep this call-sequence test isolated from the clipboard and daemon.
use bread_utils::{bread_client::BreadClient, bug::BugOptions};

#[test]
fn later_client_construction_preserves_strict_panic_reporting() {
    let mut opts = BugOptions::new("0.3.0");
    opts.panic_message = false;
    let _guard = BreadClient::connect_without_bug_reporting("clip").enable_bug_reporting(opts);

    // This is the command-client construction made after strict configuration.
    // It must not start automatic reporting or replace the configured hook.
    let _command_client = BreadClient::connect_without_bug_reporting("clip");

    let source = include_str!("../src/main.rs");
    assert!(!source.contains("BreadClient::connect(APP_ID)"));
}

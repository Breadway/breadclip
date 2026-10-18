//! Centralized client construction for breadclip's sensitive clipboard data.

use bread_utils::{bread_client::BreadClient, bug::BugOptions};

pub(super) const APP_ID: &str = "clip";

/// Create a client without starting the library's default bug reporter.
/// Every daemon event and command client must use this constructor so a later
/// client cannot replace the strict panic-reporting configuration.
pub fn client() -> BreadClient {
    BreadClient::connect_without_bug_reporting(APP_ID)
}

/// Install breadclip's strict panic reporting configuration.
pub fn enable_reporting() -> bread_utils::bug::BugGuard {
    let mut options = BugOptions::new(env!("CARGO_PKG_VERSION"));
    options.panic_message = false;
    options.forward_logs = false;
    client().enable_bug_reporting(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporting_configuration_is_strict_and_survives_later_clients() {
        assert_eq!(
            std::panic::catch_unwind(|| {
                let _guard = enable_reporting();
                let _command_client = client();
                panic!("CLIP19_SYNTHETIC_PRIVATE_TEXT_7f42");
            })
            .unwrap_err()
            .downcast_ref::<&str>(),
            Some(&"CLIP19_SYNTHETIC_PRIVATE_TEXT_7f42")
        );
    }
}

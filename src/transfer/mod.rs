//! Standalone file transfer between two dsterm instances. No DS knowledge here
//! by design: the day this module imports a distro concept, the standalone
//! contract is broken.

pub mod endpoint;
pub mod protocol;
pub mod receiver;
pub mod sender;
pub mod validate;

pub use endpoint::parse_endpoint;
pub use receiver::{run_listener, ReceiverOptions};
pub use sender::{run_sender, SenderOptions};

// Shared tool default, overridable everywhere it is used. A fixed default
// lets two bare invocations agree without an out-of-band port conversation.
pub const DEFAULT_TRANSFER_PORT: u16 = 7773;

// Long enough for a human to walk over and decide, short enough that a
// connected-but-silent sender cannot hold the listener hostage.
pub const DEFAULT_CONFIRM_TIMEOUT_SECS: u64 = 120;

// Declared metadata is advisory, never trusted for allocation: these caps
// turn a malicious or corrupt header into a fast rejection.
pub const MAX_FILES: u64 = 100_000;
pub const MAX_TOTAL_SIZE: u64 = 50 * 1024 * 1024 * 1024;
pub const MAX_PATH_COMPONENT_LEN: usize = 255;
pub const MAX_ENTRY_PATH_LEN: usize = 1024;

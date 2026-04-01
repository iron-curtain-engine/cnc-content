//! CLI command implementations.
//!
//! Each submodule handles a logical group of commands:
//! - [`status`] — informational commands (games, status, verify, detect, identify)
//! - [`install`] — content acquisition commands (download, install, clean, torrent-create)
pub mod install;
pub mod status;

//! Everything `sbx` does, with nothing that draws.
//!
//! Pulled out from under the TUI so that more than one thing can sit on top of
//! it: the terminal interface, the CLI, and -- next -- a server with a desktop
//! application on the other end of it. [`ops`] is the layer those all call, and
//! it is the reason none of them reimplement each other.
//!
//! **Nothing in here may depend on a renderer.** That is the invariant the split
//! exists to hold, and it is load-bearing rather than tidy: a core that knows
//! about ratatui cannot be linked into a server, and the drift is silent until
//! something needs it to be true. Two things used to cross the line and now do
//! not -- [`ansi`] tokenizes into its own style types instead of ratatui's, and
//! raw-mode attaching moved out to the binary, which is where the terminal being
//! handed over actually is.
//!
//! The frozen TUI still builds against this crate, which is the cheapest test
//! there is that the invariant has held.
//!
//! [`docs/architecture.md`](https://github.com/tobiaswadsethdev/sbx/blob/main/docs/architecture.md)
//! is the map of what each module owns.

pub mod ansi;
pub mod comments;
pub mod config;
pub mod doctor;
pub mod endpoints;
pub mod events;
pub mod forge;
pub mod image;
pub mod mcp;
pub mod ops;
pub mod pane;
pub mod policy;
pub mod projects;
pub mod publish;
pub mod repos;
pub mod seed;
pub mod session;
pub mod skills;
pub mod state;
pub mod status;
pub mod store;
pub mod toolchain;
pub mod update;

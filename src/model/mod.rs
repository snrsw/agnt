//! Shared session model used by the core, the TUI, and the adapters.
//!
//! The split follows the design doc: observed runtime state comes from the
//! provider (`Session`), user-owned classification lives in `SessionMeta`,
//! and `SessionView` joins the two for display.

pub mod capability;
pub mod key;
pub mod link;
pub mod session;
pub mod state;
pub mod text;

pub use capability::{Capabilities, Operation, Support};
pub use key::{IdResolveError, Provider, SessionKey, resolve_key};
pub use link::{Link, LinkKind, LinkParseError, LinkSource};
pub use session::{GitInfo, Origin, Runtime, RuntimeHandle, Session, SessionMeta, SessionView, TitleSource};
pub use state::{Grouping, RuntimeState, WorkflowState};

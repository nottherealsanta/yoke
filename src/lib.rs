//! Yoke is a small, embeddable agent harness.
//!
//! The library is the product: [`agent::run`] drives a model, runs its tool
//! calls and appends everything to the conversation you pass in. The `yoke`
//! binary (TUI and `--plain` REPL) is one consumer of this API; your own agent
//! can be another.
//!
//! The layers are small and explicit:
//!
//! - [`types`]: [`Message`](types::Message), [`Part`](types::Part),
//!   [`Request`](types::Request) and [`StreamEvent`](types::StreamEvent).
//! - [`providers`]: the [`Provider`](providers::Provider) trait, the provider
//!   table and [`make`](providers::make). [`providers::mock`] replays scripted
//!   turns, so embedders can test offline.
//! - [`tools`]: the [`Tool`](tools::Tool) trait and [`Registry`](tools::Registry),
//!   with restriction ([`Registry::only`](tools::Registry::only)) and path confinement
//!   ([`Registry::confine`](tools::Registry::confine)).
//! - [`agent`]: the loop. It sends [`Event`](agent::Event)s over a channel and
//!   owns no UI and no storage.
//! - [`tui`] and [`plain`]: the bundled consumers (kept ungated for now).
//!
//! # Example
//!
//! A whole offline turn with the mock provider:
//!
//! ```
//! use yoke::agent::{self, CancelToken};
//! use yoke::providers::mock::MockProvider;
//! use yoke::tools::Registry;
//! use yoke::types::{Message, Part, Role};
//!
//! let mut messages = vec![Message {
//!     role: Role::User,
//!     parts: vec![Part::Text { text: "hi".into() }],
//! }];
//! let provider = MockProvider::text("hello");
//! let tools = Registry::new();
//! let options = agent::Options::new("mock", "Be brief.");
//!
//! yoke::tokio::runtime::Builder::new_current_thread()
//!     .enable_all()
//!     .build()
//!     .unwrap()
//!     .block_on(async {
//!         let (tx, mut rx) = agent::event_channel();
//!         agent::run(&provider, &tools, &options, &mut messages, &CancelToken::new(), &tx)
//!             .await
//!             .unwrap();
//!         drop(tx);
//!         while rx.recv().await.is_some() {}
//!     });
//!
//! assert_eq!(messages.len(), 2);
//! ```

pub mod auth;
pub mod clock;
pub mod config;
pub mod context;
pub mod dotenv;
pub mod memory;
pub mod models;
pub mod plain;
pub mod prompt;
pub mod providers;
pub mod session;
pub mod tools;
pub mod tui;
pub mod types;

#[path = "loop.rs"]
pub mod agent;
pub(crate) use agent as r#loop;

pub use config::Config;

/// Re-exported so embedders can build a runtime and channels without adding
/// `tokio` themselves.
pub use tokio;

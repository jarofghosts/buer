//! The sequencer behind buer: the model, the scheduler, and what leaves as MPE.
//!
//! Everything here is arithmetic over plain data. The plugin crate owns the host, the window and
//! the files; this crate owns what a pattern *is* and what playing one means, so all of it can be
//! tested without a host and without a window.
//!
//! # Layout
//!
//! - [`pattern`] — notes, patterns, the bank, and the tick and lane units everything counts in
//! - [`pitch`] — lanes to notes and back, and what to call them
//! - [`mpeout`] — channel rotation and the arithmetic that gets a quarter tone out
//! - [`rng`] — a small seedable generator, so a generated pattern is reproducible
//! - [`scales`], [`scala`] — which lanes a note may land on, from a built-in set or a scale file
//! - [`generate`] — free and euclidean pattern generation

pub mod mpeout;
pub mod pattern;
pub mod generate;
pub mod pitch;
pub mod player;
pub mod rng;
pub mod scala;
pub mod scales;
pub mod smf;

pub use pattern::{Bank, Lane, LaneMask, Note, Pattern, LANES_PER_OCTAVE, SLOTS, TICKS_PER_BEAT};

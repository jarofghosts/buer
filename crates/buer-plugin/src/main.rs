//! Standalone build, for drawing on the roll and hearing it without a host.
//!
//! The CLAP plugin is the real deliverable; this is a convenience wrapper. It is also the fastest
//! way to check a quarter tone by ear: nih-plug's JACK backend registers a real `midi_output` port
//! and writes every event through `NoteEvent::as_midi` with its timing intact. Note that
//! `as_midi` has no arm for a tuning expression, so `clap` mode says nothing about pitch here —
//! which is exactly why `mpe` is the default.

use buer::Buer;
use nih_plug::prelude::*;

fn main() {
    nih_export_standalone::<Buer>();
}

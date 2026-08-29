//! What the audio thread, the editor and the background tasks all have to see.
//!
//! The audio thread never blocks and never allocates: it `try_lock`s the handoff once per block,
//! swaps in whatever is waiting, and parks what it displaced back in the same slot so that the main
//! thread does the dropping. Everything the editor only reads — where the playhead is, whether
//! anything is running — is an atomic, because a lock the editor holds for a frame is a lock the
//! audio thread would have to wait on.

use buer_core::{Bank, Note};
use nih_plug::prelude::AtomicF32;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Values in flight between the main thread and the audio thread, in both directions.
#[derive(Default)]
pub struct Handoff {
    pub incoming_bank: Option<Arc<Bank>>,
    /// Displaced banks, waiting for the main thread to drop them.
    pub retired: Vec<Arc<Bank>>,
    /// Notes the editor wants sounded once, so you hear the lane you landed on. Drawing on a
    /// twenty-four-row grid without hearing it is guesswork.
    pub auditions: Vec<Note>,
}

/// How many auditions can be waiting at once. The main thread refuses to push past this, so the
/// audio thread's drain is bounded and the vector never has to grow under it.
pub const MAX_AUDITIONS: usize = 8;

pub struct Shared {
    pub handoff: Mutex<Handoff>,
    /// Where the playhead is within the sounding pattern, in ticks.
    pub playhead: AtomicF32,
    /// Whether anything is running, so the editor knows to keep repainting.
    pub running: AtomicBool,
    /// Which slot is actually sounding. Not the same as the one being edited, and the bank strip
    /// has to show both.
    pub sounding_slot: AtomicUsize,
    /// Bumped whenever the bank is replaced by something other than the editor — a host restoring
    /// its project, a dropped file, a preset. The editor watches it to know that undoing past this
    /// point would be undoing something nobody did.
    pub generation: AtomicUsize,
    /// The last thing worth saying: what loaded, or why it did not.
    pub status: Mutex<String>,
    /// The DPI scaling the host last announced, and whether it has ever announced one. The number
    /// alone cannot tell a host that means 100 % from one that has never spoken, and those want
    /// different answers.
    pub host_dpi: AtomicF32,
    pub host_dpi_reported: AtomicBool,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            handoff: Mutex::new(Handoff {
                // Preallocated so neither side has to grow it while the other might be holding it.
                retired: Vec::with_capacity(8),
                auditions: Vec::with_capacity(MAX_AUDITIONS),
                ..Handoff::default()
            }),
            playhead: AtomicF32::new(0.0),
            running: AtomicBool::new(false),
            sounding_slot: AtomicUsize::new(0),
            generation: AtomicUsize::new(0),
            status: Mutex::new(String::new()),
            host_dpi: AtomicF32::new(1.0),
            host_dpi_reported: AtomicBool::new(false),
        }
    }
}

impl Shared {
    /// Hand a new bank to the audio thread. Main thread only.
    pub fn publish_bank(&self, bank: Arc<Bank>) {
        self.handoff.lock().incoming_bank = Some(bank);
    }

    /// The same, but announcing that the bank came from somewhere other than an edit.
    pub fn publish_restored_bank(&self, bank: Arc<Bank>) {
        self.publish_bank(bank);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Ask for a note to be sounded once. Main thread only, and dropped rather than queued when
    /// the audio thread has not kept up — a stale audition is worse than a missing one.
    pub fn audition(&self, note: Note) {
        let Some(mut handoff) = self.handoff.try_lock() else {
            return;
        };
        if handoff.auditions.len() < MAX_AUDITIONS {
            handoff.auditions.push(note);
        }
    }

    pub fn set_status(&self, message: impl Into<String>) {
        *self.status.lock() = message.into();
    }

    pub fn status(&self) -> String {
        self.status.lock().clone()
    }

    /// Drop whatever the audio thread displaced. Main thread only, once a frame.
    pub fn collect_garbage(&self) {
        let retired = {
            let mut handoff = self.handoff.lock();
            std::mem::take(&mut handoff.retired)
        };
        drop(retired);
    }
}

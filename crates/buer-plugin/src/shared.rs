//! What the audio thread, the editor and the background tasks all have to see.
//!
//! The audio thread never blocks and never allocates: it `try_lock`s the handoff once per block,
//! swaps in whatever is waiting, and parks what it displaced back in the same slot so that the main
//! thread does the dropping. Everything the editor only reads — where the playhead is, whether
//! anything is running — is an atomic, because a lock the editor holds for a frame is a lock the
//! audio thread would have to wait on.

use buer_core::{Bank, Lane, Note};
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
    /// What the audio thread took off the note input while record was armed, waiting for the
    /// editor to write it into the pattern. The other direction from everything else here.
    pub captured: Vec<Captured>,
}

/// A key going down, or coming up again, on the note input while record was armed.
///
/// Sent as the two halves rather than as a finished note: pairing them means knowing which pattern
/// is being written into, what the snap is and where the mark sits, and none of that belongs on the
/// audio thread. The editor pairs them by channel and note, the way it pairs a finger with its pad.
#[derive(Copy, Clone, Debug)]
pub struct Captured {
    pub on: bool,
    pub channel: u8,
    pub note: u8,
    /// The lane the key landed on: its note, and whatever bend its channel was carrying. Read on
    /// the way down only — an MPE channel bends while a note is held, and a key is paired with its
    /// own release by channel and note rather than by where it ended up.
    pub lane: Lane,
    /// 1..=127 on the way down, and nothing on the way up.
    pub velocity: u8,
    /// Where the playhead was when it happened, in ticks, or `None` while nothing is running.
    pub at: Option<f32>,
    /// Which slot was sounding, so the editor can tell whether that position means anything for
    /// the pattern it is writing into.
    pub slot: usize,
}

/// How many auditions can be waiting at once. The main thread refuses to push past this, so the
/// audio thread's drain is bounded and the vector never has to grow under it.
pub const MAX_AUDITIONS: usize = 8;

/// And how many captured halves can be waiting for the editor. Deep enough for a chord and a
/// couple of seconds of playing between two frames; past that the oldest recording is the one
/// nobody is watching, because the editor drains this every frame it draws.
pub const MAX_CAPTURED: usize = 128;

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
                captured: Vec::with_capacity(MAX_CAPTURED),
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

    /// Hand the editor a key the input carried. Audio thread only, and dropped rather than queued
    /// when the editor has not kept up or is not there at all — a window that is closed is a
    /// window with no working copy to write into.
    ///
    /// A dropped half costs a length rather than a note: the editor writes on the way down and only
    /// trims on the way up, so a lost release leaves a note of the length a fresh one is drawn at.
    pub fn capture(&self, event: Captured) {
        let Some(mut handoff) = self.handoff.try_lock() else {
            return;
        };
        if handoff.captured.len() < MAX_CAPTURED {
            handoff.captured.push(event);
        }
    }

    /// Take what has been captured. Main thread only, once a frame.
    ///
    /// Drained rather than taken, for the reason [`Self::collect_garbage`] is: the vector the audio
    /// thread pushes into has to come back with the room it had.
    pub fn take_captured(&self) -> Vec<Captured> {
        let mut handoff = self.handoff.lock();
        handoff.captured.drain(..).collect()
    }

    pub fn set_status(&self, message: impl Into<String>) {
        *self.status.lock() = message.into();
    }

    pub fn status(&self) -> String {
        self.status.lock().clone()
    }

    /// Drop whatever the audio thread displaced. Main thread only, once a frame.
    ///
    /// Drained rather than taken. `mem::take` leaves a *new* vector behind, with no capacity at
    /// all, and the preallocation the audio thread pushes into is gone from the first frame the
    /// editor runs — after which every handoff allocates on the audio thread. The elements are
    /// moved out under the lock and dropped outside it, which is the point of doing this in two
    /// steps: a bank is a few hundred kilobytes to free.
    pub fn collect_garbage(&self) {
        let retired: Vec<_> = {
            let mut handoff = self.handoff.lock();
            handoff.retired.drain(..).collect()
        };
        drop(retired);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collecting_the_garbage_leaves_the_room_the_audio_thread_pushes_into() {
        let shared = Shared::default();
        let capacity = shared.handoff.lock().retired.capacity();
        assert!(capacity >= 8);

        shared
            .handoff
            .lock()
            .retired
            .push(Arc::new(Bank::default()));
        shared.collect_garbage();

        let handoff = shared.handoff.lock();
        assert!(handoff.retired.is_empty());
        // The whole invariant: `poll_handoff` only pushes while there is room, so a vector that
        // came back with no capacity is one it can never park anything in without allocating.
        assert_eq!(handoff.retired.capacity(), capacity);
    }
}

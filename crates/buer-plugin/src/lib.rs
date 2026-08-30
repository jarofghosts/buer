//! buer: a quartertone piano roll that speaks MPE.
//!
//! A note effect with no audio at all — a note output port and nothing else — so the host routes
//! what it draws into whatever instrument is next in the chain.
//!
//! The one thing here that is not obvious is the event merge in [`process`](Buer::process).
//! Declaring a note *output* makes most hosts route their own note input through the plugin and
//! expect it back, so buer has two streams to send and CLAP wants them in one queue, sorted. The
//! wrapper does not sort — it pushes onto a `VecDeque` and drains it in order — so sending all of
//! the host's events and then all of ours would put a keyboard note at sample 900 in front of a
//! sequenced note at sample 0. The input is therefore flushed up to each generated event's sample,
//! interleaved, as the pattern is walked.

mod display;
mod editor;
mod midi;
mod params;
mod project;
mod shared;

use buer_core::mpeout::{Out, Voice};
use buer_core::player::{Clock, Player};
use buer_core::{Bank, TICKS_PER_BEAT};
use nih_plug::prelude::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub use params::{BuerParams, ClockParam, PitchOutParam, ZoneParam};
pub use shared::Shared;

pub struct Buer {
    params: Arc<BuerParams>,
    shared: Arc<Shared>,
    player: Player,
    /// What the audio thread is playing out of. Swapped whole; never edited in place.
    bank: Arc<Bank>,
    /// Free-running mode's own position, in ticks. Host mode reads its position instead.
    free_ticks: f64,
    /// Whether free mode was running last block, so its start can be told from its middle.
    was_free_running: bool,
    /// An input event read but not yet due. It has to survive to the next block, because the point
    /// at which we stop reading is a sample offset, not the end of the queue.
    pending_input: Option<NoteEvent<()>>,
    /// Set by `reset`, which has no context to send anything from.
    panic: bool,
    /// Set whenever the zone or the bend range changes, so the receiver is told before the next
    /// note relies on it.
    announce: bool,
    /// Notes the editor asked to hear, counting down in samples. Everything about them is sent at
    /// offset zero: an audition is a nicety and sample accuracy would only risk putting an event
    /// out of order behind one the sequencer generated.
    auditions: [Option<Audition>; MAX_AUDITIONS],
    sample_rate: f32,
}

/// How many auditions can sound at once.
const MAX_AUDITIONS: usize = 4;
/// How long one sounds for. Long enough to hear the pitch, short enough not to trail behind the
/// pointer while a note is dragged up the roll.
const AUDITION_SECONDS: f32 = 0.35;

#[derive(Copy, Clone)]
struct Audition {
    voice: Voice,
    remaining: u32,
}

impl Default for Buer {
    fn default() -> Self {
        let shared = Arc::new(Shared::default());
        Self {
            params: Arc::new(BuerParams::new(shared.clone())),
            shared,
            player: Player::new(),
            bank: Arc::new(Bank::default()),
            free_ticks: 0.0,
            was_free_running: false,
            pending_input: None,
            panic: true,
            announce: true,
            auditions: [None; MAX_AUDITIONS],
            sample_rate: 48_000.0,
        }
    }
}

impl Buer {
    /// Pick up a bank the editor has published, parking what it displaces for the editor to drop.
    fn poll_handoff(&mut self) {
        let Some(mut handoff) = self.shared.handoff.try_lock() else {
            return;
        };
        if let Some(bank) = handoff.incoming_bank.take() {
            let retired = std::mem::replace(&mut self.bank, bank);
            if handoff.retired.len() < handoff.retired.capacity().max(8) {
                handoff.retired.push(retired);
            }
        }
    }

    /// Start whatever the editor asked to hear, and stop whatever has run its course. Everything
    /// at offset zero, before the sequencer's own events, so the output queue stays sorted.
    fn auditions(&mut self, emit: &mut dyn FnMut(u32, Out), samples: u32) {
        for slot in self.auditions.iter_mut() {
            let Some(audition) = slot else { continue };
            audition.remaining = audition.remaining.saturating_sub(samples);
            if audition.remaining == 0 {
                let voice = audition.voice;
                *slot = None;
                self.player
                    .mpe()
                    .note_off(voice, &mut |event| emit(0, event));
            }
        }

        let mut wanted = [None; MAX_AUDITIONS];
        let mut count = 0;
        if let Some(mut handoff) = self.shared.handoff.try_lock() {
            while count < wanted.len() {
                match handoff.auditions.pop() {
                    Some(note) => {
                        wanted[count] = Some(note);
                        count += 1;
                    }
                    None => break,
                }
            }
            handoff.auditions.clear();
        }

        let length = (self.sample_rate * AUDITION_SECONDS) as u32;
        for note in wanted.iter().flatten() {
            let Some(slot) = self.auditions.iter().position(Option::is_none) else {
                break;
            };
            let voice = self
                .player
                .mpe()
                .note_on(note.lane, note.velocity, &mut |event| emit(0, event));
            self.auditions[slot] = Some(Audition {
                voice,
                remaining: length.max(1),
            });
        }
    }

    fn silence_auditions(&mut self, emit: &mut dyn FnMut(u32, Out)) {
        for slot in self.auditions.iter_mut() {
            if let Some(audition) = slot.take() {
                self.player
                    .mpe()
                    .note_off(audition.voice, &mut |event| emit(0, event));
            }
        }
    }

    fn publish_playhead(&self) {
        self.shared
            .playhead
            .store(self.player.position() as f32, Ordering::Relaxed);
        self.shared
            .running
            .store(self.player.is_running(), Ordering::Relaxed);
        self.shared
            .sounding_slot
            .store(self.player.slot(), Ordering::Relaxed);
    }
}

/// Push every input event at or before `upto`, then stop, keeping the one that was too late.
fn flush_input<C: ProcessContext<Buer>>(
    context: &mut C,
    pending: &mut Option<NoteEvent<()>>,
    upto: u32,
    pass: bool,
) {
    loop {
        let Some(event) = pending.take().or_else(|| context.next_event()) else {
            return;
        };
        if event.timing() > upto {
            *pending = Some(event);
            return;
        }
        if pass {
            context.send_event(event);
        }
    }
}

/// Put a sample offset on one of the player's events and hand it to the host.
fn send<C: ProcessContext<Buer>>(context: &mut C, timing: u32, event: Out) {
    let event = match event {
        Out::Bend { channel, value } => NoteEvent::MidiPitchBend {
            timing,
            channel,
            value,
        },
        Out::NoteOn {
            voice_id,
            channel,
            note,
            velocity,
        } => NoteEvent::NoteOn {
            timing,
            voice_id: Some(voice_id),
            channel,
            note,
            velocity,
        },
        Out::NoteOff {
            voice_id,
            channel,
            note,
            velocity,
        } => NoteEvent::NoteOff {
            timing,
            voice_id: Some(voice_id),
            channel,
            note,
            velocity,
        },
        Out::Tuning {
            voice_id,
            channel,
            note,
            semitones,
        } => NoteEvent::PolyTuning {
            timing,
            voice_id: Some(voice_id),
            channel,
            note,
            tuning: semitones,
        },
        Out::Cc { channel, cc, value } => NoteEvent::MidiCC {
            timing,
            channel,
            cc,
            value: value as f32 / 127.0,
        },
    };
    context.send_event(event);
}

impl Plugin for Buer {
    const NAME: &'static str = "buer";
    const VENDOR: &'static str = "grimoire.supply";
    const URL: &'static str = "https://github.com/jarofghosts/buer";
    const EMAIL: &'static str = "decapitron@gmail.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    /// None at all. buer makes no sound; it tells something else what to make.
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[];

    /// `MidiCCs` rather than `Basic` at both ends: the bend that carries a quarter tone and the RPNs
    /// that declare the zone are both CC-level messages, and the wrapper drops them below this.
    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone(), self.shared.clone())
    }

    fn initialize(
        &mut self,
        _layout: &AudioIOLayout,
        config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        // State may have been restored before we were activated.
        self.poll_handoff();
        self.sample_rate = config.sample_rate;
        self.announce = true;
        true
    }

    fn reset(&mut self) {
        self.panic = true;
        self.free_ticks = 0.0;
        self.was_free_running = false;
        self.pending_input = None;
        self.announce = true;
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        self.poll_handoff();

        let samples = buffer.samples() as u32;
        let params = &self.params;
        let pass = params.pass_through.value();

        // Everything read off the parameters and the transport first, so that `context` is free for
        // events from here on.
        let free = params.clock.value() == ClockParam::Free;
        let looping = params.looping.value();
        let wanted_slot = (params.pattern.value() - 1).max(0) as usize;

        let transport = context.transport();
        let sample_rate = transport.sample_rate as f64;
        let host_playing = transport.playing;
        let host_beats = transport.pos_beats().unwrap_or(0.0);
        let host_tempo = transport.tempo.unwrap_or(0.0);

        let tempo = if free {
            params.free_tempo.value() as f64
        } else {
            host_tempo
        };
        // A host that reports no tempo — clap-validator's process tests do exactly this — moves
        // nothing, which is the honest answer and also keeps the divisions below finite.
        let ticks_per_sample = if sample_rate > 0.0 {
            tempo / 60.0 * TICKS_PER_BEAT as f64 / sample_rate
        } else {
            0.0
        };

        let playing = if free {
            params.play.value()
        } else {
            host_playing
        };
        if free && playing && !self.was_free_running {
            self.free_ticks = 0.0;
        }
        self.was_free_running = free && playing;

        let ticks = if free {
            self.free_ticks
        } else {
            host_beats * TICKS_PER_BEAT as f64
        };

        self.player.set_gate(params.gate.value());
        self.player.select(wanted_slot);
        let changed = self.player.mpe().configure(
            params.mpe_zone.value().into(),
            params.pitch_out.value().into(),
            params.bend_range.value() as f32,
        );
        self.player
            .mpe()
            .set_transpose(params.transpose.value() as i16);
        self.announce |= changed;

        let bank = self.bank.clone();
        let mut pending = self.pending_input.take();

        if self.panic {
            self.silence_auditions(&mut |offset, event| {
                flush_input(context, &mut pending, offset, pass);
                send(context, offset, event);
            });
            self.player.silence(&mut |offset, event| {
                flush_input(context, &mut pending, offset, pass);
                send(context, offset, event);
            });
            self.panic = false;
        }

        if std::mem::take(&mut self.announce) {
            flush_input(context, &mut pending, 0, pass);
            let mpe = self.player.mpe();
            let mut events = |event| send(context, 0, event);
            mpe.announce(&mut events);
        }

        self.auditions(
            &mut |offset, event| {
                flush_input(context, &mut pending, offset, pass);
                send(context, offset, event);
            },
            samples,
        );

        self.player.process(
            &bank,
            Clock {
                playing,
                ticks,
                ticks_per_sample,
            },
            samples,
            looping,
            &mut |offset, event| {
                flush_input(context, &mut pending, offset, pass);
                send(context, offset, event);
            },
        );

        // Whatever the host sent after the last note we generated still has to go out.
        flush_input(context, &mut pending, u32::MAX, pass);
        self.pending_input = pending;

        if free && playing {
            self.free_ticks += samples as f64 * ticks_per_sample;
        }

        self.publish_playhead();
        ProcessStatus::Normal
    }
}

impl ClapPlugin for Buer {
    const CLAP_ID: &'static str = "supply.grimoire.buer";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("a quartertone piano roll that speaks mpe");
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::NoteEffect,
        ClapFeature::Utility,
        ClapFeature::Custom("sequencer"),
    ];
}

nih_export_clap!(Buer);

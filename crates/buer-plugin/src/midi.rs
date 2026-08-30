//! Standard MIDI files, the file half: `midly` on one side, [`buer_core::smf`] on the other.
//!
//! Everything musical — what a bend means, which channel a note goes out on — belongs to the core
//! crate and is shared with the audio thread. This file knows about chunks, delta times and running
//! status, and nothing else.

use buer_core::pattern::{Bank, Note, Pattern, TICKS_PER_BEAT};
use buer_core::smf::{self, Message, Report, Timed};
use midly::num::{u15, u24, u28, u4, u7};
use midly::{
    Format, Header, MetaMessage, MidiMessage, PitchBend, Smf, Timing, Track, TrackEvent,
    TrackEventKind,
};
use std::path::Path;

/// What came out of a file.
#[derive(Debug)]
pub struct Imported {
    /// One entry per track that had any notes, in the file's own order.
    pub tracks: Vec<(String, Vec<Note>)>,
    pub report: Report,
    /// The file's first tempo, if it declared one.
    pub tempo: Option<f32>,
}

pub fn read(path: &Path) -> Result<Imported, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("could not read that file: {error}"))?;
    let file = Smf::parse(&bytes).map_err(|error| format!("not a readable midi file: {error}"))?;

    // A timecode file counts in frames rather than beats. Converting it needs the tempo, which is
    // in the file but may change; rather than guess, say so.
    let ppq = match file.header.timing {
        Timing::Metrical(ppq) => ppq.as_int() as u32,
        Timing::Timecode(..) => {
            return Err(
                "that file counts time in smpte frames rather than beats, which buer \
                        cannot place on a bar grid"
                    .to_string(),
            )
        }
    };

    let mut tracks = Vec::new();
    let mut report = Report::default();
    let mut tempo = None;

    for track in file.tracks {
        let mut name = String::new();
        let mut events = Vec::new();
        let mut tick = 0u32;

        for event in track {
            tick = tick.saturating_add(event.delta.as_int());
            match event.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(bytes)) if name.is_empty() => {
                    name = String::from_utf8_lossy(bytes).trim().to_string();
                }
                TrackEventKind::Meta(MetaMessage::Tempo(micros)) if tempo.is_none() => {
                    let micros = micros.as_int() as f32;
                    if micros > 0.0 {
                        tempo = Some(60_000_000.0 / micros);
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    let channel = channel.as_int();
                    let message = match message {
                        MidiMessage::NoteOn { key, vel } => Message::NoteOn {
                            channel,
                            note: key.as_int(),
                            velocity: vel.as_int(),
                        },
                        MidiMessage::NoteOff { key, .. } => Message::NoteOff {
                            channel,
                            note: key.as_int(),
                        },
                        MidiMessage::PitchBend { bend } => Message::Bend {
                            channel,
                            word: bend.0.as_int(),
                        },
                        MidiMessage::Controller { controller, value } => Message::Cc {
                            channel,
                            cc: controller.as_int(),
                            value: value.as_int(),
                        },
                        _ => continue,
                    };
                    events.push(Timed { tick, message });
                }
                _ => {}
            }
        }

        let (notes, track_report) = smf::import(&events, ppq);
        report.notes += track_report.notes;
        report.worst_cents = report.worst_cents.max(track_report.worst_cents);
        report.bent_while_sounding += track_report.bent_while_sounding;
        report.inexact_ticks += track_report.inexact_ticks;
        if !notes.is_empty() {
            tracks.push((name, notes));
        }
    }

    if tracks.is_empty() {
        return Err("there are no notes in that file".to_string());
    }

    Ok(Imported {
        tracks,
        report,
        tempo,
    })
}

/// Fill a bank from an imported file: one track per slot, in order.
///
/// Tracks map to slots rather than being merged, because a multitrack file is already the shape a
/// bank is, and merging would throw that away for nothing.
pub fn into_bank(imported: &Imported, bank: &mut Bank) -> usize {
    let mut filled = 0;
    for (slot, (name, notes)) in imported.tracks.iter().take(buer_core::SLOTS).enumerate() {
        let end = notes.iter().map(|note| note.end()).max().unwrap_or(0);
        // Round up to a whole bar of 4/4, so the loop lands where the music does.
        let bar = TICKS_PER_BEAT * 4;
        let bars = end.div_ceil(bar).max(1);

        let mut pattern = Pattern::empty(
            if name.is_empty() {
                format!("{}", slot + 1)
            } else {
                name.clone()
            },
            bars,
            4,
        );
        pattern.replace(notes.iter().copied());
        *bank.pattern_mut(slot) = pattern;
        filled += 1;
    }
    filled
}

/// Say plainly what the file made us do that it did not ask for.
pub fn describe(imported: &Imported, filled: usize) -> String {
    let report = &imported.report;
    let mut said = format!(
        "imported {} notes into {filled} {}",
        report.notes,
        if filled == 1 { "pattern" } else { "patterns" }
    );
    if report.worst_cents > 1.0 {
        said.push_str(&format!(
            "; the furthest note moved {:.0} cents onto its lane",
            report.worst_cents
        ));
    }
    if report.bent_while_sounding > 0 {
        said.push_str(&format!(
            "; {} bent while sounding, and buer kept the pitch each started at",
            report.bent_while_sounding
        ));
    }
    if report.inexact_ticks > 0 {
        said.push_str("; some events did not divide evenly and moved by under a tick");
    }
    said
}

/// What to write, and how.
pub struct Export {
    pub slot: usize,
    /// Every slot that has notes in it, rather than just [`Self::slot`].
    pub all: bool,
    pub tempo: f32,
    pub zone: buer_core::mpeout::Zone,
    pub range: f32,
    pub repeats: u32,
}

/// Write a pattern, or the whole bank, as a format-1 file with the bends baked in.
pub fn write(path: &Path, bank: &Bank, export: &Export) -> Result<(), String> {
    // The file counts in buer's own ticks, so nothing is rescaled on the way out and a pattern that
    // goes out and comes back is the pattern that left.
    let mut file = Smf::new(Header::new(
        Format::Parallel,
        Timing::Metrical(u15::new(TICKS_PER_BEAT as u16)),
    ));

    let micros = (60_000_000.0 / export.tempo.max(1.0)).round() as u32;
    let meta = |message| TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(message),
    };
    file.tracks.push(Track::from(vec![
        meta(MetaMessage::Tempo(u24::new(micros.min(0x00FF_FFFF)))),
        meta(MetaMessage::TimeSignature(4, 2, 24, 8)),
        meta(MetaMessage::EndOfTrack),
    ]));

    let slots: Vec<usize> = if export.all {
        (0..buer_core::SLOTS)
            .filter(|slot| !bank.pattern(*slot).is_empty())
            .collect()
    } else {
        vec![export.slot]
    };
    if slots.is_empty() {
        return Err("there is nothing to export".to_string());
    }

    for slot in slots {
        let pattern = bank.pattern(slot);
        let events = smf::export(pattern, export.zone, export.range, export.repeats);
        let mut track = Track::new();
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(pattern.name.as_bytes())),
        });

        let mut at = 0u32;
        for event in &events {
            let delta = event.tick.saturating_sub(at);
            at = event.tick;
            track.push(TrackEvent {
                delta: u28::new(delta),
                kind: kind(event.message),
            });
        }
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        file.tracks.push(track);
    }

    file.save(path)
        .map_err(|error| format!("could not write that file: {error}"))
}

fn kind(message: Message) -> TrackEventKind<'static> {
    let (channel, message) = match message {
        Message::NoteOn {
            channel,
            note,
            velocity,
        } => (
            channel,
            MidiMessage::NoteOn {
                key: u7::new(note.min(127)),
                vel: u7::new(velocity.min(127)),
            },
        ),
        Message::NoteOff { channel, note } => (
            channel,
            MidiMessage::NoteOff {
                key: u7::new(note.min(127)),
                vel: u7::new(0),
            },
        ),
        Message::Bend { channel, word } => (
            channel,
            MidiMessage::PitchBend {
                bend: PitchBend(midly::num::u14::new(word.min(16383))),
            },
        ),
        Message::Cc { channel, cc, value } => (
            channel,
            MidiMessage::Controller {
                controller: u7::new(cc.min(127)),
                value: u7::new(value.min(127)),
            },
        ),
    };
    TrackEventKind::Midi {
        channel: u4::new(channel.min(15)),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Export {
        fn at(slot: usize, tempo: f32) -> Self {
            Self {
                slot,
                all: false,
                tempo,
                zone: buer_core::mpeout::Zone::Lower,
                range: 48.0,
                repeats: 1,
            }
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("buer-test-{}-{name}.mid", std::process::id()));
        path
    }

    fn a_bank() -> Bank {
        let mut bank = Bank::default();
        let pattern = bank.pattern_mut(0);
        pattern.name = "rast".to_string();
        pattern.insert(Note::new(0, 480, 120, 100));
        pattern.insert(Note::new(480, 240, 127, 64));
        pattern.insert(Note::new(960, 960, 133, 127));
        bank
    }

    #[test]
    fn a_pattern_written_and_read_back_is_the_same_pattern() {
        let bank = a_bank();
        let path = scratch("roundtrip");
        write(&path, &bank, &Export::at(0, 120.0)).unwrap();

        let imported = read(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(imported.tracks.len(), 1);
        assert_eq!(imported.tracks[0].0, "rast");
        assert_eq!(imported.report.inexact_ticks, 0);
        assert_eq!(imported.report.bent_while_sounding, 0);

        let before = bank.pattern(0).notes();
        let after = &imported.tracks[0].1;
        assert_eq!(after.len(), before.len());
        for (before, after) in before.iter().zip(after.iter()) {
            assert_eq!(
                (before.start, before.length, before.lane, before.velocity),
                (after.start, after.length, after.lane, after.velocity)
            );
        }
    }

    #[test]
    fn a_quarter_tone_survives_the_file() {
        let mut bank = Bank::default();
        bank.pattern_mut(0).insert(Note::new(0, 480, 121, 100));
        let path = scratch("quartertone");
        write(&path, &bank, &Export::at(0, 120.0)).unwrap();
        let imported = read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(imported.tracks[0].1[0].lane, 121);
    }

    #[test]
    fn the_tempo_written_is_the_tempo_read() {
        let path = scratch("tempo");
        write(&path, &a_bank(), &Export::at(0, 140.0)).unwrap();
        let imported = read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!((imported.tempo.unwrap() - 140.0).abs() < 0.1);
    }

    #[test]
    fn a_bank_of_patterns_comes_back_as_a_bank_of_patterns() {
        let mut bank = a_bank();
        let second = bank.pattern_mut(3);
        second.name = "bayati".to_string();
        second.insert(Note::new(0, 480, 130, 90));

        let path = scratch("bank");
        write(
            &path,
            &bank,
            &Export {
                all: true,
                ..Export::at(0, 120.0)
            },
        )
        .unwrap();
        let imported = read(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(imported.tracks.len(), 2);
        let mut back = Bank::default();
        assert_eq!(into_bank(&imported, &mut back), 2);
        assert_eq!(back.pattern(0).name, "rast");
        assert_eq!(back.pattern(1).name, "bayati");
    }

    #[test]
    fn a_pattern_becomes_a_whole_number_of_bars() {
        let mut bank = Bank::default();
        // A note ending a beat into the second bar.
        bank.pattern_mut(0)
            .insert(Note::new(TICKS_PER_BEAT * 4, 480, 120, 100));
        let path = scratch("bars");
        write(&path, &bank, &Export::at(0, 120.0)).unwrap();
        let imported = read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let mut back = Bank::default();
        into_bank(&imported, &mut back);
        assert_eq!(back.pattern(0).length, TICKS_PER_BEAT * 8);
    }

    #[test]
    fn something_that_is_not_a_midi_file_says_so() {
        let path = scratch("garbage");
        std::fs::write(&path, b"this is not a midi file").unwrap();
        let error = read(&path).unwrap_err();
        std::fs::remove_file(&path).ok();
        assert!(error.contains("not a readable midi file"), "{error}");
    }
}

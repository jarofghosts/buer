# buer

A CLAP piano roll and sequencer that works in **quarter tones** and sends what you draw out as
**MPE**, so any synth on the next track can play it.

A conventional piano roll has twelve lanes to the octave, and a quarter tone is at best a detune
buried in a value field. buer draws twenty-four, so a quarter tone is a row you can point at — and
it speaks both dialects a CLAP host understands, so what you point at is what you hear.

Named for the demon Buer, a wheel of five goat legs around a lion's head, which is also the mark in
the top left. It turns while the sequencer runs.

## Installing

```
cargo xtask bundle buer --release      # -> target/bundled/buer.clap
```

Copy `buer.clap` to `~/.clap/` (Linux), `~/Library/Audio/Plug-Ins/CLAP/` (macOS) or
`%COMMONPROGRAMFILES%\CLAP\` (Windows). On macOS it is a directory rather than a file, so keep it
whole; a downloaded one is neither signed nor notarised and needs clearing by hand,
`xattr -dr com.apple.quarantine buer.clap`.

buer is a **note effect**: it has no audio at all, only a note output port. Put it before an
instrument and the host routes its notes into it.

There is also a standalone build, which registers a real JACK MIDI port:

```
cargo run -p buer --features standalone --bin buer-standalone -- --backend jack
```

## The roll

Twenty-four rows to the octave. The twelve semitone rows carry the keyboard's own black and white;
the twelve quarter-tone rows between them are tinted and separated by a fainter line, and the
keyboard down the left draws them as narrow stubs between the keys. Every row is the same height, so
the arithmetic that turns a y into a lane is a division — which is what keeps dragging, the marquee,
the velocity lane and the toggle column all agreeing with each other.

- **Draw** by clicking empty grid — that places a note of whatever `length` is set to — or by
  dragging, where the note appears on the press and the drag sets its length instead. Hold **alt**
  to bypass the snap and be exact to the tick.
- **Move** by dragging a note, **shift** to lock to one axis. **Resize** from its right edge.
- **Select** by clicking a note, **shift**-click to add, **shift**-drag on empty grid for a
  marquee, `ctrl+a` for all, **escape** for none.
- **Delete** with a right-click or the delete key.
- **Velocity** is the lane under the roll — one bar per note, drag it. Velocity also sets how solid
  a note is drawn, so a phrase's dynamics are legible without looking down.
- **Scroll** to pan, **shift**-scroll sideways, **ctrl**-scroll to zoom in time and
  **ctrl+shift**-scroll to zoom the lanes. Zooming keeps whatever is under the pointer still.
- Anything you draw, move or transpose **sounds once** as you do it. Drawing on a twenty-four-row
  grid without hearing it is guesswork.
- `ctrl+z` and `ctrl+shift+z`. A whole drag is one step, and a gesture that changed nothing is not a
  step at all.

**names** decides how much of the keyboard down the left is labelled: `octaves` for each c alone,
`notes` for every semitone with the quarter tones left blank, or `lanes` for all twenty-four. It is
saved with the instance. Zoomed out past the point where a row can hold a name it narrows back to
the octaves on its own, because a column of overlapping text is worse than no names at all.

**snap** and **length** are the two chip rows above the roll. `length: draw` means a note starts one
snap long and the drag sets the rest; any other choice makes a click exactly that long. Dragging
always overrides either.

The roll scales with `ui scale` like everything else — the zoom is held in screen points, so it is
rescaled by hand when the interface is. Without that it is the one part of the window that does not
grow, and at 200 % a note comes out four pixels tall and reads as nothing at all.

## Getting a quarter tone out

There are two ways to say "fifty cents above this note" to a CLAP host, and they are not
interchangeable. `pitch out` and what follows from it — the MPE zone, the bend range — are behind
**settings…** in the header, along with the scale: set once for an instance, and then left alone
where they are not taking room from the roll.

- **mpe** (the default) gives every sounding note a channel of its own and bends that channel. It
  arrives everywhere — every MIDI-dialect host, every hardware synth, and the standalone build's own
  JACK port. Lower or upper zone, and a bend range that is announced on the wire (RPN 0) along with
  the MPE Configuration Message that tells a receiver the zone exists at all.
- **clap** sends a single tuning note expression, which is exact and understood by rather fewer
  instruments. `NoteEvent::as_midi` has no arm for a tuning expression, so this mode says nothing
  about pitch anywhere the MIDI dialect is in use — including the standalone build.
- **both** is for a host that passes one dialect and drops the other. It is not the default: an
  instrument that honours *both* plays a semitone sharp, not a quarter tone.

The arithmetic, since it is the whole point: a quarter tone is half a semitone, which at MPE's
default ±48 is 85 of the 8192 units above centre — the bend word `8277`, sounding 49.8 cents. At ±2
it is `10240`, exact. A fifth of a cent is well under anything audible, so ±48 stays the default
because it is what MPE synths assume. The bend always goes **before** the note-on; reversed, every
quarter tone arrives as a semitone that slides.

## Patterns

Sixteen slots in one instance. The strip shows each as a thumbnail of its notes; the **fill** is the
slot being edited and the **outline** is the slot sounding, which are frequently not the same one.
`follow` ties them together.

`pattern` is a real parameter, so a host can record and automate a change of slot — which is what
makes a bank more than a filing cabinet. A change takes effect **at the loop point**, so nothing is
cut in half; while stopped it takes effect at once.

## Transport

**host** follows the host's own position, read from the transport every block rather than counted
here, so scrubbing, host loops and locates all just work. **free** runs at buer's own tempo and its
own `play` switch, so it keeps going when the host is stopped — `play` is a parameter and not a
button, because the audio thread cannot write to its own parameters.

`gate` is what share of its written length a note actually sounds for, always clipped at the loop
point: a note allowed to ring across the wrap would meet its own retrigger on the same lane.

## Which lanes a note may land on

The column of twenty-four toggles down the left of the roll **is** the constraint, and lanes it
excludes are tinted out across the whole roll. A scale is *stamped* into it rather than intersected
with it live — with a live intersection, turning a lane on by hand does nothing whenever the scale
excludes it, and a toggle that visibly moves and changes nothing is the worst thing a per-lane
control can do. Stamping costs one thing, that choosing another scale discards hand edits, and
`ctrl+z` pays that back.

Three sources stamp into it:

- the **built-in set** — chromatic 24 and 12, the modes, and the maqam families (rast, bayati, saba,
  sikah, huzam, hijaz, nawa athar) as 24-EDO expresses them, which is an approximation the theory
  itself makes;
- a **Scala `.scl`** file, dropped on the editor or loaded from **settings…**. Every degree lands on its
  nearest lane and the editor says how far the worst one had to move — nothing is dropped quietly;
- your own hand, on the toggles.

The built-in set and the `.scl` loader are both in **settings…**; the toggles are always there,
because they are the constraint rather than a way of writing one.

The constraint is per pattern, so a bank can hold a bayati beside a rast. The scale file is per
instance, because there is only one of it, and it is stored in the state so a project carries its own
tuning.

## Randomising

One panel, two shapes, sharing everything else.

- **free** rolls for a note on every step of a grid, with a density and a rest chance — how busy, and
  how ragged.
- **euclidean** spreads a number of pulses over a number of steps as evenly as whole steps allow.
  This is Bjorklund's actual pairing algorithm, not the shorter modulo form that agrees with it on
  many inputs and turns E(5,8) into a rotation of the cinquillo rather than the cinquillo.

Both take the pattern's lane constraint, a register, a velocity range and a **seed**. The seed is
shown, saved, and only ever rolled by the dice button — generation is a pure function of it, so the
number on screen is always the number that made what you are looking at, and "keep this one and
nudge the density" works.

## Files

**save…** and **load…** write a `.buer` file: the plugin's own state in a small versioned envelope,
so a project cannot drift out of step with what the plugin persists. A host that saves its project
has already saved all of it; this is the copy you can name and move between hosts. Dropping one on
the editor loads it.

**import midi…** reads a standard MIDI file. Bends are read against the range the file declares —
RPN 0, and the MPE Configuration Message if it has one, since reading an MPE file at ±2 puts every
note a long way out — and each note's pitch at its *start* becomes a lane. The editor says what the
file made it do: how far the furthest note had to move, how many bent after they had already
started, whether the ticks divided evenly. Nothing is quantised to a grid. Tracks become slots, and
the file's tempo becomes the free-running one.

**export midi…** writes the pattern back out with the bends baked in, at buer's own resolution, run
through the *same* channel allocator the audio thread uses — so an exported file and a played
pattern cannot disagree. Export and import round-trip exactly; there is a test that says so.

## Ticks

Everything is counted in ticks at 1920 to the quarter note. `1920 = 2^7 · 3 · 5`: triplets and
quintuplets divide exactly, every resolution a MIDI file arrives at divides it (384 included, which
960 does not), and it fits an SMF's metrical header so nothing is rescaled on the way out.

## Building

```
cargo fmt --all --check          # what CI checks
cargo test --workspace           # and this
cargo xtask bundle buer --release
clap-validator validate target/bundled/buer.clap
```

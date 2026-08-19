# Iteration 4 — what changes, and in what order

Read against `Yarngo Studio.dc.html` (4a–4i) and the screenshots in
`design/iteration-4/`. Iterations 1–3 stay where they still apply: **2c and 2d
remain the recording and check states** of the enrolment sheet, and 3a–3d are
untouched. What follows replaces the workspace.

The design's own summary of the change, from its section note:

> The left side lists clips and nothing else. A voice is not something you
> browse next to your work — it is a property of the clip you are making, so it
> moves right, next to the model and the delivery settings that belong to the
> same clip.

Three ideas drive everything below.

1. **Clips are the only list.** Voices leave the sidebar.
2. **One frame that does not move.** Header, one card, one row of actions.
   State changes happen *inside* the card; the buttons underneath stay put.
3. **A clip is a thing with a name and a voice**, not just the last thing
   generated. It can exist before it has audio.

---

## What is already right

The title bar, the settings window, the model switcher, the enrolment sheet
(2b–2d), and the whole setup wizard are unaffected. Inside the workspace these
survive: the 236px sidebar width, the sidebar footer's runtime line, the
composer's 22px/24px padding, the counts row, the offline note, the seed
concept, and the player's waveform renderer.

---

## A. Sidebar — clips only

| | design | now |
|---|---|---|
| top | **New clip** button, 34px, `#1F1C19`, `add` glyph, white text | nothing |
| heading | `CLIPS` with the count right-aligned in mono `#B0A79B`, 26px row | `VOICES` … `CLIPS`, no counts |
| rows | fixed **52px**, `padding:9px`, `gap:2px` between | variable height |
| row subtitle | `0:31 · My voice` — duration and voice | `0:31 · 14:02` — duration and time |
| draft row | `edit_note` `#8F4406`, `draft · not generated`, `#FFF3E6` | no drafts exist |
| generating row | `progress_activity`, `generating · 4s left`, and a 3px bar pinned inside the row (`left:9 right:9 bottom:5`) | a separate "Generating…" row above the list |
| empty | dashed card, "Nothing here yet. Everything you generate lands in this list and stays on this machine." | similar, but under a VOICES section |
| footer line 2 | `4 clips · 12 MB` | `4 models on disk · 16.2 GB` |

The voices section, the "Add a voice" row and the sample row all leave.

## B. Title bar — an inspector toggle

A 26×24 button, `1px #E4DCD0`, radius 6, holding `right_panel_open` /
`right_panel_close` at 18px `#857D72`, between the model pill and the gear.

## C. Composer header — 46px, fixed

- Clip name at Sora 19 semibold with a 17px `edit` glyph beside it, on a 26px row.
- Under it, one 12.5px line that changes with state:
  `Default voice · Fast` → `Named from your first words · Default voice · Fast`
  → `0:16 · made 14:02 today · Default voice · Fast`.
- **When the inspector is closed**, two 30px chips sit right: `record_voice_over`
  + voice name + chevron, and `tune` + the model name. Both open the inspector.
  When it is open, the chips are gone.

## D. The card — one frame, four states

Always: a **76px strip**, then the text area, then a **34px** counts row. The
card itself never changes size.

| state | strip | text area | actions row (38px) |
|---|---|---|---|
| **4a** empty | 40px outlined circle, `#C4BBAE` play, "The audio appears here when you generate", a hairline rule, `0:00` | placeholder with a 1.5px caret | Generate (disabled) + "Write something first. The default voice is ready — nothing to record." |
| **4b** writing | same placeholder strip | the text | Generate (accent) + "About 7 seconds to make." |
| **4c** generating | 36px `#FFF3E6` circle with `graphic_eq`, "Generating — about 4 seconds left", mono "10 of 16 seconds of audio · 2.5× realtime", Cancel, and a 4px bar under it | `lock` + `TEXT — LOCKED WHILE IT RUNS`, text in `#5F594F` | **Start another clip** + "This one keeps running in the list." |
| **4d** finished | 40px solid `#FF8A1F` play, a waveform whose bars are `flex:1` (fills the width, unlike 1f's fixed 3px bars), `0:04 / 0:16` | `description` + `TEXT IT WAS MADE FROM` + an `Edit text` button; footer line "Editing the text makes another take…" | Generate again (primary, `refresh`) + Save as WAV + Copy audio |

## E. Inspector — 300px, `THIS CLIP`

Replaces today's 280px clip/voice inspector entirely. Header is a 44px bar with
the mono label and `right_panel_close`. Body, 14px padding, 15px gaps:

- **VOICE** — the bundled default first (`check_circle` `#8F4406`, `#FFF3E6`
  when chosen, "bundled · works with every model", a `play_circle` to hear it),
  then `YOURS · N`, then a dashed **Record a voice** row with `~1 min`, then one
  explanatory line that changes with the case.
- **MODEL** — a 36px select showing the model name, and under it a capability
  line: `check` + "Uses recorded voices".
- **DELIVERY** — the `Seed` row, with a `casino` reroll. Speed is not built;
  see the decision below.

While generating, a `#FFF3E6` banner tops the panel: "Settings are fixed for
this run."

## F. Rename (4e)

Inline in the header: a 26px field, `1.5px #171717`, min-width 300, selected
text on `#FFD9AE`, with `Enter to save · Esc to cancel` beside it. The actions
row does not move. The row in the sidebar renames too.

## G. Recording from the inspector (4h → 4i)

**Record a voice** opens the same sheet as first-run setup. On save the clip
being written switches to the new voice, the header subtitle changes to
`My voice · Fast`, the generate hint becomes "Now in your voice. The default
voice is still there if you want it back.", and the panel shows a
`check_circle` banner: "Voice saved and selected for this clip."

---

## What this needs from the engine, and what it cannot have yet

These gate parts of the above. **Three need a decision before I build them.**

### 1. Clips must exist before they have audio

Drafts, names, and "a clip is a property bag" all need the clip record to be
created when you start writing, not when generation finishes. Needs a `name`
field, a `rename_clip` call, and a draft state the list can show. Straight
work, no unknowns.

### 2. `Speed 1.0×` — **decided: dropped**

The engine has no speed parameter; `DOTS_GEN` is guidance scale, speaker scale,
patch cap, EOS threshold and template. Nothing there changes rate. The honest
options:

- **Drop it.** Delivery shows Seed alone and the chip reads `Fast`, not
  `Fast · 1.0×`.
- **Time-stretch after generation** (WSOLA or a phase vocoder, pitch
  preserved). Real, but it is a new audio dependency and it degrades quality at
  the edges of the range.
- **Resample.** Cheap and wrong — it moves the pitch with the rate.

**Decided: dropped.** DELIVERY shows Seed alone and the header chip reads
`Fast`, not `Fast · 1.0×`. Nothing on screen claims a setting that does
nothing.

### 3. Per-model built-in voices (4f, and the third case of 4g) — **decided: not now**

4f shows `VOICE · KOKORO'S OWN` with named voices — Aria, Kola, Noor, each with
a character line, and `+5 more`. Nothing in the app has this: every model in the
catalogue is a cloning model that speaks as itself when given no reference, and
Kokoro is not in the catalogue at all.

Two halves, and they are separable:

- **The locked case is real and worth building now** — when a model cannot
  clone, your recorded voices stay listed, locked, with "Kokoro cannot copy a
  recording. Fast and Best quality can." and a **Switch to Fast** button. That
  is exactly right; it just cannot be *reached* today, because all five
  catalogue models report `supports_cloning: true`.
- **The named-voice catalogue is a data change** — a per-model list of voice
  ids and descriptions, plus a way to preview each. Worth building only
  alongside a model that actually has them.

**Decided: not built.** The panel shows the bundled default voice and yours,
which is 4b and the first two panels of 4g. 4f is left for whenever a model
with named voices actually joins the catalogue; until then it is unreachable
and building it would be building against a guess.

### 4. The default voice becomes the initial selection

Today the app selects the first enrolled voice and treats "no voice" as a
fallback. The design makes the bundled default a real, first-class, chosen-out-
of-the-box option — "nothing to record" is the first-run promise. That is a
small change to selection defaults and to how the sample row is worded, and it
is worth doing regardless of the rest.

---

## Build order, and where it got to

1. **Sidebar → clips only** — done. New clip button, count, 52px rows, draft and
   generating rows with the inline progress bar, `N clips · N MB` footer.
2. **Clip records** — done. `name` on the clip, seeded from its first words by
   the sidecar; `rename_clip` through the engine; drafts held by the app, one
   per clip being written, each with its own text, voice, model and seed.
3. **One frame** — done. Header / card / actions, with the strip, the text area
   and the counts row inside the card. 4a, 4b, 4c and 4d all render in it.
4. **Inspector** — done. Default voice first, then yours, then Record a voice;
   model with its capability line; seed with a reroll.
5. **Title-bar toggle and the header chips** — done.
6. **Record from the inspector** — wired: the panel's Record a voice opens the
   enrolment sheet, and saving selects the new voice for the clip and shows the
   banner. The sheet itself is the one verified in iterations 2b–2d.
7. **The locked no-clone case and a model's own voices** — left out, per the
   decisions above.

Verified by running the app: empty (4a), writing (4b), generating (4c), a
finished clip (4d), renaming from the header with the actions row staying put
(4e), the inspector open and closed, and the transport starting a fresh clip at
`0:00` rather than at its end.


---

## Second pass — what the audit found, and what is left

Four agents read 4a–4i against the code and the screenshots. Most of it is
applied; this is what remains, and why.

### Structural — now built

**A finished clip's inspector is a different panel**, and is. 4d shows
**MADE WITH** — the voice, model and seed that produced the reading you are
hearing, with a `Reuse` action — and **TAKES**, every reading of the clip.

So **a clip holds several takes**, and "Generate again" adds one rather than a
second row in the list. The sidecar stores them newest-first and migrates older
records on read; a take whose audio has gone is dropped, and a clip with none
left is not listed. The selection carries the take as well as the clip.

**The `more_horiz` row menu** carries rename, duplicate and delete, anchored to
the click rather than to the row so the list can scroll under it. Duplicate
copies the audio too — a copy you can change or delete without touching the
original.

### Applied, for the record

The three real bugs (invisible saved-voice banner, Enter/Escape bound globally,
`cancel_rename` firing on every Escape), filled glyphs, title-bar geometry and
the toggle's open state, the placeholder tint, the empty-state and placeholder
and offline copy, a fresh draft called "New clip" with no pencil and no row, the
footer note per state, the name field's own styling and mono key caps, 40
waveform bars filling their row, the seconds-left figure in the sidebar row, the
sheet mounted inside the body with its in-context copy and a disabled Generate
behind it, the sidebar footer's generating line, and `Edit text`'s 15px glyph.

### Not applied, deliberately

- **4c's read-only inspector.** The design greys the whole panel while a clip
  runs — 38px locked voice row, no chevron on the model, bare seed value. The
  implementation states it in a banner instead and leaves the controls live,
  because changing them there sets up the *next* take rather than corrupting the
  running one. Worth revisiting if it reads as ambiguous.
- **4c's privacy note in the panel.** The design repeats "Runs on this machine.
  Works offline." at the foot of the inspector while generating. It is already
  said in the composer on the screens with room for it.
- **4a's `1 model on disk · 1.2 GB` footer line.** The design shows the model
  line on the empty screen and the clips line everywhere else. One line that
  changes subject with the screen is harder to read than one that always
  reports the same thing; the clips count is the one that belongs beside a list
  of clips.

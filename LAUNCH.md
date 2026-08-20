# Launch plan

Written 19 Aug 2026 against the repository as it stands. Everything below was
checked in the code or on the wire, not assumed; where something is a decision
rather than a fact, it says so.

---

## What "launch" has to mean first

The work splits three ways depending on the answer, so it is worth settling
before anything else:

1. **You and a handful of people, by hand** — a `.dmg` passed around. Needs
   signing and notarization and little else.
2. **A public download** from a page you control. Adds the page, an icon, an
   About pane with licences, a privacy statement, and some way to ship a fix.
3. **The Mac App Store.** Adds sandboxing, which the current design fights: the
   app downloads a CPython runtime and pip-installs into it at first run. That
   is not a small change — assume it rules the Store out for now.

The rest of this assumes **2**, and marks what 1 could skip.

---

## Hard blockers

### 1. Signing needs an Apple Developer account — **the path is wired, the
### certificate is not**

`package.sh` now signs, then notarizes and staples when a Developer ID is
configured, and says loudly when it is not:

> This build is ad-hoc signed. It runs here, and Gatekeeper will refuse it on
> any other Mac.

Credentials come from either `APPLE_NOTARY_PROFILE` (a stored `notarytool`
profile) or `APPLE_ID` + `APPLE_TEAM_ID` + `APPLE_PASSWORD`, alongside
`APPLE_SIGNING_IDENTITY`. A release build also runs its own checks — deep
signature verify, the audio-input entitlement present, the icon present, and a
Gatekeeper assessment — because each of those fails silently otherwise.

**What is still needed: an Apple Developer account and a Developer ID
Application certificate**, then one run with the environment set, and a check
that the stapled artifact opens on a Mac that has never seen it.

### 2. Naming — **all lowercase**

The brand guidelines said "use Yarngo Sentence case in all writing; the
lowercase yarngo is the visual wordmark". That rule is superseded: it is
`yarngo studio` in writing too, including the bundle name and the microphone
prompt. Two things deliberately keep the old spelling — the data directory,
because renaming it would strand every existing install's voices, clips and
consent log, and `Yarngo Studio.dc.html`, which is a filename in the design
bundle.

### 3. The application icon — **done**

Cut from the brand mark by `scripts/make-app-icon.py`, following the
guidelines' own rule for it: "The aperture fills the tile", on an orange field
rather than the in-product warm white. Apple's macOS geometry — 1024 canvas,
824 tile, 185 corner radius — and wired into the packager, so the bundle now
carries `YarngoStudio.icns` and names it in its plist.

### 4. First run needs the network, and asks for a lot of it

A machine that already has MLX and `mlx-speech` skips the first two entirely —
the app looks for an interpreter that can import the package before offering to
download one. For everyone else, in order: ~350 MB of CPython, then
`pip install mlx-speech` and its dependency tree, then a 3.4 GB model. The offline path covers only the first of those —
"Install from a file" takes the interpreter archive and the card says plainly
that the packages still come from the network.

This is not wrong, but it is the first impression, and it is a long one. Decide
whether launch ships as-is with the wait stated honestly (it currently is), or
whether the runtime and packages are pre-bundled into the download. Bundling
would make the download several gigabytes and cross-compiling the wheels is its
own project.

**`mlx-speech` is live on PyPI and the model repos resolve on Hugging Face** —
both checked. The dependency is real, not aspirational.

### 5. Windows — same models, different runtime

**The requirement is that Windows offers the same recommended models, not an
equivalent list.** "Fast" is dots.tts MF on both, "Best quality" is dots.tts
SOAR on both. A user moving between machines hears the same voice, and a clip
made on one opens on the other naming a model that is really there.

That rules out CrispASR as the Windows engine. It was the settled choice — ggml,
MIT, one native binary, no CPython download at all — but its TTS families are
vibevoice, chatterbox, f5-tts, TADA, irodori, kokoro, qwen3-tts, cosyvoice3-tts,
omnivoice, piper, melotts and fastpitch. **dots.tts is not among them**, so
choosing it means changing the model, which is the one thing that must not
change.

**The route that keeps the models is PyTorch.** dots.tts is upstream a PyTorch
model — `rednote-hilab/dots.tts-mf` and `dots.tts-soar`, revision-pinned — and
`mlx-speech` is a port of it, not its origin. The same checkpoints run under
torch on Windows, CPU or CUDA. So the shape of the app does not change at all:
the Python sidecar stays, the protocol stays, the catalogue stays. What changes
is the package installed into the runtime — `mlx-speech` becomes the torch
path — and the runtime installer already knows how to fetch CPython for
`x86_64-pc-windows-msvc`.

What this costs, and none of it is hidden:

- **A much larger runtime.** MLX is small; torch is not. CPU-only is a few
  hundred megabytes, a CUDA build is gigabytes. The first-run wait that is
  already the weakest part of macOS gets worse on Windows.
- **Speed has to be measured, not assumed.** MLX on Apple silicon is not a
  guide to torch on a Windows CPU. A machine without CUDA may be too slow to
  ship, in which case the honest answer is a hardware requirement, not a
  quieter model.
- **Numerical parity is a real question.** The same checkpoint under a different
  runtime is not automatically the same audio. This is a cheaper test than the
  original bake-off, though, and a more decisive one: run the Nigerian
  references through torch dots.tts and compare against the MLX output that was
  already validated. It either matches or it does not.

CrispASR does not disappear — it stays the fallback if torch turns out to be
unshippable on Windows, and its consent log, watermarking and C2PA signing are
worth borrowing regardless. But it is no longer the plan.

### 6. Apple silicon only — **now said, before anything is downloaded**

MLX is Apple-silicon only, so the runtime installer being generic was a trap:
an Intel Mac would download several hundred megabytes, install them happily,
and fail at `import mlx_speech` with a traceback naming none of it.

`runtime::host_supported()` refuses first, and the setup screen states the
reason where the install button would be:

> Yarngo Studio needs an Apple silicon Mac — M1 or later. The speech models run
> on Apple's MLX, which Intel Macs cannot use.

Two tests hold it: nothing is downloaded or unpacked on an unsupported host,
and the message names what is missing rather than saying "unsupported".

---

## Functional gaps

### Iteration 4 — **finished**

Takes and the row menu are both built and verified. A clip holds several
readings; generating again adds one rather than a second row. Rename, duplicate
and delete are on the row, and duplicate copies the audio as well as the record.

### Settings — **Storage and About are built**

Models, Voices, Storage and About are real. **General, Audio and Runtime** are
still named-but-empty, which the window is honest about, and none of them
blocks a launch.

Storage measures what is on disk rather than estimating it, off the UI thread,
and names the one folder it all lives in. About states the privacy position,
explains the consent record and links to its log, and lists the licences of the
models actually installed.

### No way to ship a fix

No auto-update, no version check, no crash reporting. For option 1 that is fine.
For a public download, decide between a Sparkle-style updater and simply telling
people to re-download — but decide, because the first shipped build will have
something wrong with it.

---

## Quality gates before any of it

### Tests — **done, as a first layer**

Thirty-eight, where there were none. They cover the sidecar protocol (the
handshake, that every request field is sent, and that an engine refusal, a bad
reply and a dead process stay three distinct things), the runtime installer,
the audio bars, and file import against real fixtures in four formats. One runs
against the real `engine.py`, ignored by default.

They have already earned it: adding `clip_id` to the request broke the suite
rather than the app. What is still uncovered:

- The clip/draft state machine in `clips.rs`: start, select, rename, generate,
  and what happens to a draft when its voice is deleted. It needs a gpui
  context, so it needs a test harness first.
- The takes migration in the sidecar — an old clip record becoming a clip with
  one take. Currently only exercised by running the app.

### Verify on a machine that is not this one

Every check so far has been on the development Mac, where the runtime, the
models and the microphone grant are already in place. Before launch, a clean
user account or a second machine, from download to first clip, with the network
throttled at least once.

---

## The legal and ethical surface

This is the part that is unusual for a desktop app and worth getting right.

- **Consent is recorded per voice** — the wording as it stood, the app version,
  the source, and a SHA-256 of the audio the claim was made about, appended to
  `consent.log` and never rewritten. That is a real foundation.
- **Model licences are commercially clean**: Apache-2.0 and MIT across the
  catalogue, and the sidecar's own comment records that non-commercial models
  were deliberately left out. Surface them in About.
- **A privacy statement** should be short and true: nothing leaves the machine
  except model downloads, clips and recordings stay in
  `~/Library/Application Support/Yarngo Studio`, and deleting that folder
  removes everything. The setup screen already says a version of this.
- **Voice-likeness terms.** Decide what the product says about cloning someone
  else's voice, beyond the per-voice checkbox. The checkbox protects the record;
  the terms protect the position.

---

## Where this stands

Done: tests, iteration 4, the Storage and About panes, the icon, the signing
and notarization path, and the Apple-silicon gate.

Left, in order:

1. **A Developer ID certificate**, then one signed and notarized build, opened
   on a Mac that has never seen it. This is the only remaining thing that stops
   the app being handed to another person.
2. **Terms for voice likeness**, beyond the per-voice checkbox. The checkbox
   protects the record; terms protect the position.
3. **Decide updates** before the first build goes out, not after.
4. The first-run story, if it is changing from "state the wait honestly".
5. **If Windows is in scope**, prove torch dots.tts first: same checkpoints,
   same references, compared against the validated MLX output. Everything else
   about the port is wiring; that comparison is the part that can fail.

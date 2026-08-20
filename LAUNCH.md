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

### 5. Windows — same models, and three layers that must not be conflated

**The requirement is that Windows offers the same recommended models, not an
equivalent list.** "Fast" is dots.tts MF on both, "Best quality" is dots.tts
SOAR on both. Substituting f5-tts or qwen3-tts under a label called "Fast"
would throw away the checkpoint that was validated on Nigerian speakers, which
is the whole reason the eval was run.

Three separate layers, and every earlier confusion here came from merging two
of them:

| Layer | Values |
| --- | --- |
| Product label | Fast, Best quality |
| Model identity | dots.tts MF, dots.tts SOAR |
| Runtime | MLX, PyTorch, ggml/CrispASR |

The catalogue already splits the first two — `ModelSpec.label` against
`ModelSpec.name`. The third is not represented yet and should be: a model row
saying "dots.tts MF · Runtime: CUDA" is the honest version of the same product
on two machines.

#### CrispASR supports dots.tts SOAR — an earlier note here said otherwise

That note was wrong, and wrong through bad method: the TTS backends were read
off `--help`, which lists flags rather than backends. The binary already in
`voice-clone-bench/crispasr/` contains a `DotsTtsBackend`, a `dots-tts` backend
id, and hard-coded download URLs to `cstr/dots-tts-soar-GGUF`.

What it does **not** contain is MF. Every dots GGUF filename compiled into it is
`dots-tts-soar-*` — core, speaker encoder, vocoder — and there is no MF artifact
or MF URL. The `CRISPASR_DOTS_FM_*` environment variables are flow matching,
which is SOAR's DiT head; the one `meanflow` string in the binary belongs to
`s3gen`, a Chatterbox vocoder. So there is no latent MF support waiting to be
switched on.

That makes ggml a real portable path for **Best quality** — CPU, CUDA, Vulkan or
Metal, no Python at all, ~2.2 GB at mixed Q4_K — and no path for **Fast**.

#### PyTorch keeps both checkpoints, but Windows is unproven — two concrete blockers

An earlier note here claimed "the same checkpoints run under torch on Windows".
That was an inference stated as a fact. Checking upstream
(`studio-dots-ai/dots.tts`) turns up two specific problems:

1. **A packaging blocker, not an inference blocker.** dots.tts requires
   `WeTextProcessing`, which requires `pynini>=2.1.6`, and pynini 2.1.7
   publishes manylinux wheels only — no `win_amd64`, no `win32` — with its own
   guidance sending Windows users to conda-forge or WSL. But that dependency
   buys **text normalisation, which the upstream runtime defaults to off**
   (`normalize_text: bool = False`). So it blocks `pip install dots.tts`; it
   does not block generating audio. Nothing else in the dependency list is
   Linux-bound: no triton, flash-attn, deepspeed, vllm or xformers, and no
   manylinux pins.

   Dropping it from the install list is not sufficient on its own.
   `dots_tts/utils/text.py` imports `tn.chinese.normalizer` and
   `tn.english.normalizer` **at module scope**, and `runtime.py` imports from
   that module at module scope in turn — so a runtime without the package fails
   at import, before any generation. The fix is to make those two imports lazy,
   which is a few lines and worth sending upstream rather than carrying.

2. **No single Python version can serve both backends.** `mlx-speech` declares
   `requires-python = ">=3.13"`; upstream `dots.tts` declares `>=3.10,<3.13`.
   The two are mutually exclusive, so moving the runtime to 3.12 wholesale would
   break the macOS path that currently works. The version is now a property of
   the runtime pack (`runtime::MLX` at 3.13.15, `runtime::TORCH` at 3.12.14),
   both from the same pinned python-build-standalone release, which publishes
   3.12.14 for `x86_64-pc-windows-msvc` as well. Three tests hold the split, and
   one of them asserts that nothing selects the unproven pack.

Upstream's trove classifiers list POSIX::Linux and MacOS and omit Windows, which
means untested rather than impossible — but combined with pynini it is enough
that "torch on Windows" stays a hypothesis until a machine says otherwise.

#### The spike, and nothing larger

One Windows 11 machine with an NVIDIA GPU:

1. CPython 3.12.14 — already the `TORCH` pack's pin.
2. Install PyTorch CUDA.
3. Install dots.tts **without** `WeTextProcessing`, with the two `tn.*` imports
   in `utils/text.py` made lazy.
4. Run with `normalize_text = False`, which is the default anyway.
5. Load dots.tts MF, clone the Nigerian reference already used for validation,
   generate one clip.
6. Same again with SOAR.

Pass is two valid clips. That is the whole test, and it decides the shape of the
port. If it fails, diagnose the failing operation — do not reopen the runtime
comparison.

If it passes: macOS on MLX, Windows and Linux on PyTorch CUDA, and ggml as the
portable and low-memory fallback wherever it is supported — which today means
SOAR only.

ONNX is not a route yet. There is no maintained ONNX build of the dots pipeline,
and exporting it means exporting the whole iterative stack — semantic encoder,
Qwen backbone, AR patch generation, flow-matching DiT, vocoder, speaker
conditioning — not one graph. Worth revisiting only if both routes above fail.

#### Runtimes are infrastructure; models are the choice

Adding `runtime` to the model metadata is right, but it does not belong under
every row in the picker. What a person chooses is "Fast · dots.tts MF · 3.4 GB".
Which engine executes it is the app's business, and belongs where runtimes are
managed — one line saying what is installed and what it runs on. The picker is
already close to reading like a technical tool; a backend name on every row
would push it over.

#### Not every machine offers every model

Step-Audio-EditX asks for roughly 12 GB of VRAM and tests only on Linux, and
LongCat-AudioDiT 3.5B is about 15 GB unquantized. Neither should be promised to
a low-spec Windows machine. The Models pane already separates what is on this
machine from what is not; it needs a third state — offered, but not runnable
here, with the reason — so a model the hardware cannot run is visibly
unavailable rather than a download that disappoints.

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
5. **If Windows is in scope**, run the one spike in section 5 — and settle the
   CPython pin regardless, because 3.13.15 cannot install upstream dots.tts on
   any platform.

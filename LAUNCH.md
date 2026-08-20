# Launch plan

Written 19 Aug 2026, revised 20 Aug 2026, against the repository as it
stands. Everything below was
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
   app downloads a CPython runtime and builds the speech environment inside it
   at first run. That
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

yarngo installs its own runtime and does not borrow the machine's. It used to:
a `python3` on PATH that could import the speech package was used as-is,
skipping the download. That saved a download and cost determinism — two people
would be running different versions of the engine and its whole dependency
tree, which is the thing the committed lock exists to prevent — and it risked
Apple's command-line-tools dialog appearing over our own setup screen, since a
bare `/usr/bin/python3` is a stub on a Mac without them. `YARNGO_PYTHON` remains
as a developer override. So, in order: ~350 MB of CPython, then `uv sync
--frozen --no-dev` builds the speech environment from the pack's committed
lock, then a 3.4 GB model. The offline path covers only the first of those —
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

Five separate layers, and every earlier confusion here came from merging two
of them:

| Layer | Values |
| --- | --- |
| Product label | Fast, Best quality |
| Logical model | dots.tts MF, dots.tts SOAR |
| Model artifact | `mf/mlx-int8`, upstream BF16, `dots-tts-soar-q4_k.gguf` |
| Runtime | MLX, PyTorch, ggml/CrispASR |
| Hardware backend | Metal, CUDA, Vulkan, CPU |

The artifact row is why "same model" needs saying carefully: the MLX build of
MF is an int8 conversion, and a torch build would run upstream's checkpoint —
the same logical model, different bytes, and potentially different numerics
and cloning quality. The catalogue already half-records this (`precision`, and
separate ids for `mlx-int8` against `mlx-base`); what will make it matter is
MADE WITH provenance, once a clip can have been made by either artifact. Not an
abstraction to build now — a distinction to stop collapsing in prose.

The catalogue already splits the first two — `ModelSpec.label` against
`ModelSpec.name`. Runtime and hardware backend are not represented yet, and
they are why **runtime selection keys on host capabilities, not any one field**.
The OS alone cannot separate two Windows machines (NVIDIA wants torch CUDA;
AMD has no torch path), and the accelerator alone cannot separate two CUDA
machines (CUDA does not say whether the Windows or the Linux pack fits). The
eventual resolver reads OS, architecture, accelerator and what each runtime
actually supports — replacing `if target_os` with an equally wrong
`if gpu_vendor` would repeat the same mistake one layer down.

When the second runtime actually lands, which model runs on which runtime on
which hardware becomes a small table resolved at startup — data, not an
if-chain. It is deliberately not built today: with one shipped runtime the
table has one row, and an abstraction over one case is how the last two
architecture mistakes here started.

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

2. **uv installs the packages, for every pack; the interpreter pin stays ours.**
   uv cannot fetch our interpreter — its downloadable versions are compiled into
   the uv binary, and the one tested here (0.9.8) tops out at 3.13.9 against our
   pinned 3.13.15. Pinning through uv would tie the Python version to uv's
   release cadence. So the split is one tool per job, applied to both packs:
   we fetch the interpreter, `uv sync --frozen` builds the environment beside it
   from a committed lock. Proven end to end — a real install from an empty
   directory reaches a venv that imports `mlx_speech`, and `uv sync` against our
   own unpacked 3.13.15 took 1.6 seconds.

   Two things this buys beyond consistency. **Installs became reproducible**:
   `pip install --upgrade` resolved fresh on every machine, so two people
   installing a week apart got different dependency trees; the lock pins all 26
   with hashes, and the shipped command is `uv sync --frozen --no-dev` so uv
   cannot quietly re-lock on the way in. And **the pynini exclusion became
   declarative**: `[tool.uv] exclude-dependencies`, scoped to `dots-tts`, so the
   manifest says "yarngo packages dots.tts without its normaliser" rather than
   forbidding the package to everything. The lock records the exclusion and
   resolves 109 packages instead of 112 — pynini and WeTextProcessing are absent
   rather than present-and-gated, and `dots-tts==0.3.1` still resolves alongside
   torch 2.13.0. The scoping was checked by pointing it at the wrong package,
   which brings them straight back.

   uv itself is pinned at 0.12.5 and its published SHA-256 is verified before it
   is unpacked — an unverified executable that then gets signed with our own
   identity would undo both the signature's meaning and the locks'.

   `uv add` is the dev-time way to edit a pack manifest, not part of the install
   path. `uv tool` is for CLI tools in isolated environments; the sidecar is
   imported as a library, so it has no use here.

3. **No single Python version can serve both backends.** `mlx-speech` declares
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

#### The parity half of the spike is already done, on this Mac

Upstream selects `cuda` else `cpu`, and CPU torch runs on Apple silicon — so
"do the upstream checkpoints generate under torch, and is it the same voice"
never needed NVIDIA. It ran here on 20 Aug 2026, through the real sidecar over
the real protocol (`scripts/parity_probe.py`), against the torch pack's own
interpreter and committed lock:

- **Install**: `uv sync --frozen` on the pack, 5.17 GB upstream MF checkpoint
  (`dots-studio/dots.tts-mf@25c53fb…` — the very revision the MLX artifact was
  converted from), model loaded in 58 s on CPU.
- **Output**: the full target sentence, word for word, at 48 kHz. Speaker
  similarity through the eval's own embedder: **torch-vs-MLX 0.994**,
  torch-vs-reference 0.963, MLX-vs-reference 0.955. The two runtimes are the
  same voice to the speaker encoder.
- **Speed**: CPU RTF ≈ 55–77. CUDA is not an optimisation, it is the product;
  a CPU-only torch runtime is confirmed unshippable, which the capability map
  already said.
- **Two live findings, both about the reference, chased to ground:**

  *A hesitant reference clones its hesitation.* Both backends, identically:
  the bench reference ends in "…first um" plus a pause, and every clip made
  from it opened with a filler and a multi-second hold. With a cleanly-ending
  reference the artifact vanishes on both. The app's fixed enrolment script
  ends firmly, so recorded voices are protected; imported references are not,
  which the import warning already covers.

  *Upstream drops leading words — a genuine dots-tts 0.3.1 bug, found and
  patched.* With a clean reference and its exact transcript, torch
  deterministically lost the first clause of the target (two seeds, same
  result) while MLX, same inputs, kept every word. Root cause, from reading
  both implementations: **upstream disagrees with itself by one span.** Its
  model drops the final partial patch of prompt latents (the tail is padding,
  not speech), but its runtime schedule ceils — reserving one more prompt span
  than the model prefills, and the orphan span lands exactly where the
  target's first words belong. The sidecar aligns the runtime with upstream's
  own model (version-guarded to the pinned 0.3.1, mirroring the validated MLX
  port's `ceil − 1`). Measured on the user's enrolled voice: every word
  intact, likeness 0.974 against the reference and **0.993 against MLX** —
  where speaker-only conditioning, the interim workaround, had scored an
  audibly-worse 0.944. On an unrecognised upstream version the backend falls
  back to speaker-only rather than reintroduce the bug. Worth filing upstream
  with exactly this diagnosis.

What is left for the Windows machine is now only what a Mac cannot answer:
`uv sync` of this lock on Windows, CUDA initialisation, generation speed on
real hardware, and the installer. The model question is closed.

#### The spike, and nothing larger

One Windows 11 machine with an NVIDIA GPU:

1. CPython 3.12.14 — the `TORCH` pack's pin; the installer already fetches it
   for `x86_64-pc-windows-msvc`.
2. `uv sync --frozen` on the torch pack. The lock resolves `torch==2.8.0+cu129`
   for `win_amd64` — pinned deliberately: newer torch (2.13) publishes **no
   Windows wheel** on the CUDA index, so an unpinned resolve produces a lock
   that looks complete and fails only on a Windows machine. A test now holds
   the pin.
3. `python scripts/parity_probe.py` with the same reference and an accurate
   transcript — the identical harness that already passed on macOS CPU. The
   `tn` stub ships in the sidecar (`dots_torch.py`), so no upstream patching.
4. Confirm CUDA is actually in use (upstream logs its device) and record the
   RTF. Then SOAR, same steps.

Pass is two valid clips at a usable speed, **plus a likeness listen** against
the same enrolled voice's MLX clip. The macOS CPU harness already bounded the
gap: across two seeds, torch-fp32-CPU scored 0.969–0.974 against the enrolled
reference where MLX-int8-Metal scored 0.980, with the words intact and the
speaker-embedding input verified identical (both backends read the same first
10 s) — a small, consistent residual attributable to the numeric profile, and
audible to the voice's owner. CUDA runs bf16, a third profile no Mac clip
represents, so the ear test on real hardware is part of the pass, not an
afterthought. If it fails, diagnose the failing operation — do not reopen the
runtime comparison.

If it passes: macOS on MLX, Windows and Linux NVIDIA on PyTorch CUDA, and ggml
as the portable fallback where it is supported — which today means SOAR only.

**Be precise about what "Windows support" means at first.** The same-models
requirement can only be met where MF has a runtime, and MF today runs on MLX
and — pending the spike — torch CUDA. So the first Windows release means
**Windows x86-64 with a supported NVIDIA GPU**. A Windows AMD or CPU-only
machine currently has no MF path at all: ggml is SOAR-only. Those machines
follow later, either as an explicitly reduced tier or once MF has a portable
implementation — shipping them under the same "Fast / Best quality" promise
today would silently break the requirement this section opens with.

On the ggml fallback's footprint: what is established is small weights —
roughly a 2.2 GB Q4_K core plus a ~330 MB vocoder — which makes an 8 GB
machine plausible, not proven. RAM headroom and generation speed there are
unmeasured, so it is a smaller-footprint fallback, not yet a low-memory
promise.

**Torch is the transitional Windows runtime, not a candidate for the Mac.**
Upstream's device selection is `cuda` if available, else `cpu` — verified in
`runtime.py`, with no `mps` path and no device parameter to override it. On
Apple silicon the reference implementation runs on CPU. So "torch everywhere
for consistency" is not an option that exists; MLX on the Mac is settled twice
over.

ExecuTorch and ONNX are the same category: a better eventual endpoint — a
native runtime with no CPython in the product — behind the same unbuilt port.
Both need the whole iterative stack exported: semantic encoder, autoregressive
Qwen backbone with its KV cache, patch generation loop, 16-step flow-matching
DiT, vocoder, speaker conditioning. `torch.export` is weakest exactly at
dynamic control flow, which is most of that list. Parked, not planned: the day
it matters, the first probe is whether `torch.export` survives the Qwen
backbone — an afternoon that decides whether the rest is worth anyone's month.
The `SpeechEngine` trait is what keeps torch replaceable by one of these
without the app noticing.

#### Prior art, checked in the clones rather than believed

Superwhisper and FluidVoice both ship cross-platform voice apps; both are
cloned under `third-party/` (gitignored) and were read, not summarised. The
first finding is negative and saves effort: **neither publishes Windows
inference source.** Superwhisper's repo is a download page — macOS, Windows
x64, Windows ARM, iOS installers, no code. FluidVoice's macOS app is real open
source, but its `windows-main` branch is a single README: "The Windows source
code is not published here." There is nothing to copy; what is learnable is
deployment architecture, from release artifacts and release notes.

What their shipping practice confirms or adds:

- **The app installer stays small; heavy things arrive on demand.** FluidVoice's
  Windows installer is 11 MB. Runtimes and models download after. Our macOS
  build already works this way; the torch runtime must never be bundled into a
  Windows installer.
- **Accelerator packs are separate artifacts.** parakeet.cpp publishes
  `bin-win-{cpu,vulkan,cuda}-x64` plus a separate `cudart-…-win-cuda-x64.zip`;
  FluidVoice ships an on-demand CUDA overlay (with app-local MSVC runtime
  files) over a CPU/Vulkan base, as its own release tag. For a future ggml
  runtime that pattern is literal. What follows is **yarngo design derived
  from that pattern, not the pattern itself**: for a torch pack, a CUDA
  variant and a CPU variant are different locks against different wheel
  indexes, not one pack with a flag. MSVC runtime files are a shipping
  concern, not a spike concern.
- **Their bugs are the checklist.** FluidVoice shipped, then fixed: older GPUs
  attempting an unsupported CUDA runtime instead of falling back (0.0.8) — so
  the capability probe must verify the runtime actually loads, not trust the
  vendor string; and model updates leaving the previous version on disk,
  never reclaimed (0.0.9) — so replacing a model or runtime must delete what
  it replaced, and our Storage pane would make that leak visible.
- **Models update independently of the app** — "Update available" on the model
  row, the current one keeps working until the user chooses. Source: release
  tag `windows-v0.0.9`, published 11 Aug 2026, **marked prerelease** — which
  hides it from the releases page's default view, so checking the page alone
  concludes 0.0.8 is latest. The stale-model fix quoted above is in that tag's
  notes verbatim. This feeds the open "no way to ship a fix" decision: the app
  updater and the model/runtime updater are two different mechanisms.
- **Feature parity is not a launch gate.** Superwhisper's Windows build openly
  lags its Mac features. A Windows v1 that does clone → Fast/Best quality →
  generate → play/export, with panes missing, is a legitimate release.
- **They did not universalise the Mac engine.** FluidVoice's Mac app declares
  `platforms: [.macOS("15.0")]`, links CoreAudio, pins FluidAudio to a branch —
  and Windows is a separate implementation anyway. MLX staying Mac-only is the
  normal pattern, not a compromise.

#### Windows, locked — against source that is actually public

Superwhisper and FluidVoice ship Windows but hide its source. Four projects do
not, and together they cover every piece of the Windows design. All four are
cloned under `third-party/` and the claims below were read in their code, not
in their marketing.

**The torch stack has shipped, at scale, with our exact choices.** Buzz
(21k stars) pins `requires-python = ">=3.12,<3.13"` — the same pin as our
torch pack — uses uv with a committed `uv.lock` (315 packages), and routes
torch per platform in `[tool.uv.sources]`: PyPI on macOS, the
`download.pytorch.org/whl/cu129` index elsewhere, NVIDIA's NGC index for the
CUDA runtime libraries. Its Windows CI runs
`uv pip install torch==2.8.0+cu129 torchaudio==2.8.0+cu129` and its installer
collects `msvc-runtime` app-locally. Python 3.12 + uv + torch CUDA on Windows
is not our hypothesis any more; it is Buzz's production configuration. What
remains ours to prove is dots.tts specifically — which is what the spike is.

**The process boundary we already have is the one the field converged on.**
Sona describes itself as "designed to be spawned and owned by another
process"; Vibe *migrated to* that after starting with in-process FFI, which is
the direction of travel worth noticing. Our sidecar protocol is already this
boundary. The lock is: **the protocol is the runtime interface.** The torch
pack reuses `engine.py` over the same stdin/stdout JSON; a future ggml runtime
implements the same protocol as a native process; the app cannot tell them
apart. No HTTP needed — the transport is already ours.

**Probing must happen in the child process, because the failure mode is dying
before main.** Handy's Cargo.toml documents why: the prebuilt ONNX Runtime is
compiled `/arch:AVX2` and executes BMI2 **in a static initializer** — on a
pre-Haswell CPU the process crashes at startup, before any capability check
could run. (The relayed claim that Handy "removed DirectML" is not what the
code shows — DirectML is still an option; the real lesson is the initializer
crash.) Our runtime-as-child-process shape already contains the fix: launching
the runtime *is* the probe, and a crash kills the sidecar, not yarngo.
OpenWhispr then shows the fallback done properly, at two levels: any startup
rejection falls back to CPU, and so does the *first request* failing — "CUDA
aborting on an unsupported GPU at the first kernel launch". READY is earned by
a loaded model answering, never by a vendor string.

**Runtime binaries are pinned and digest-checked, like ours.** OpenWhispr pins
its runtime release tag with per-tag SHA-256 digests — "Pinned so untested
future binaries never auto-ship" — which is the same rule our uv fetch and
locks already follow. When native runtime packs exist, they get the same
treatment. One more OpenWhispr comment worth keeping: a stale flag "must be
dropped, or ggml silently runs on CPU forever" — degraded-but-working needs to
be *visible*, or nobody ever finds out.

**The Windows traps checklist, each from a wound in public:**

- 260-character path limit breaks native builds even with long paths enabled —
  MSBuild tooling ignores the setting; Handy compiles through a short NTFS
  junction (`BUILD.md`).
- Non-ASCII user paths break discovery — Handy #1187, "fix cyrillic (unicode)
  path problems". Test under `C:\Users\Jérôme`, not only `C:\Users\dev`.
- Prebuilt binaries carry an ISA baseline — AVX2 crashes pre-Haswell CPUs at
  startup (Handy). Applies to any wheel or GGUF runtime we ship.
- MSVC runtime files ship app-local — Buzz collects `msvc-runtime`; FluidVoice
  ships VC++ files inside its CUDA overlay.

**Selection sequence, locked** (implemented when the second Windows runtime
exists; v1 needs only its one row):

```text
model chosen → detect host capabilities → find installed compatible runtime
→ else offer the recommended one, download, verify digest
→ launch the runtime process → capability probe inside it → load model
→ READY — and on failure at any stage: fall back, say so, never silently
```

**Windows v1 capability map** — the target row is the spike; ggml rows are
Best-quality-only until an MF port exists:

| Windows hardware | Runtime | Fast (MF) | Best quality (SOAR) |
| --- | --- | --- | --- |
| NVIDIA x64 | torch CUDA | target | target |
| AMD / Intel GPU x64 | ggml Vulkan | no path | yes |
| CPU-only x64 | ggml CPU | no path | plausible, unmeasured |
| ARM64 | — | no path | undecided |

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

Forty-five, where there were none. They cover the sidecar protocol (the
handshake, that every request field is sent, and that an engine refusal, a bad
reply and a dead process stay three distinct things), the runtime installer and
its packs (per-pack Python pins, the committed locks, the scoped pynini
exclusion, that nothing selects the unproven pack), the audio bars, and file
import against real fixtures in four formats. One runs against the real
`engine.py`, ignored by default.

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
5. **If Windows is in scope**, run the Windows NVIDIA spike in section 5. The
   Python question is closed — each pack pins its own.

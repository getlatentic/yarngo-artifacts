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

**Done, 20 Aug 2026.** The certificate was already held for another app —
Developer ID Application is issued per team, not per app, so one covers both.
`yarngo studio_0.1.0-alpha.1_aarch64.dmg`, 16.7 MB, signed by
`Developer ID Application: Tosin Amuda (94SW7AUBMX)`, notarized and stapled.
Gatekeeper reports `source=Notarized Developer ID · accepted` for the image,
for the app, and for the app mounted from inside the image, and all three
staple-validate offline.

Producing it exposed two faults in the packaging script, both of which only
appear the first time a real Developer ID is used:

- `notarytool submit` accepts only a `.zip`, `.pkg` or `.dmg` — never a bare
  `.app`. Bundles are now zipped with `ditto` for submission, which preserves
  the symlinks and extended attributes a signature depends on, and the ticket
  is stapled onto the original directory.
- The image was built *before* notarization, so it carried a copy of the app
  made before its ticket existed. The app is now notarized and stapled first
  and the image built around it, so a bundle dragged out of the image validates
  offline rather than only while the machine can reach Apple.

What remains is opening it on a Mac that has never seen it — the one check this
machine cannot perform for itself.

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
as a developer override. So, in order: a 42 MB `uv`, pinned and checked against
a digest compiled into the binary; ~350 MB of CPython; `uv sync --frozen
--no-dev` building the speech environment from the pack's committed lock; then
a 3.4 GB model.

uv is fetched here rather than bundled. Inside the application it was 42 MB of
a 37.5 MB download — larger than the application itself — for a tool used once,
by someone already committed to downloading 350 MB. Fetching it took the disk
image from **37.5 MB to 16.7 MB**. It lands in the runtime directory it serves,
so it leaves with the runtime, and its digest is compiled in rather than read
from the `.sha256` published beside the archive: that file proves the download
survived the wire, not that it is the build this app was tested against. The offline path covers only the first of those —
"Install from a file" takes the interpreter archive and the card says plainly
that the packages still come from the network.

This is not wrong, but it is the first impression, and it is a long one. Decide
whether launch ships as-is with the wait stated honestly (it currently is), or
whether the runtime and packages are pre-bundled into the download. Bundling
would make the download several gigabytes and cross-compiling the wheels is its
own project.

**`mlx-speech` is live on PyPI and the model repos resolve on Hugging Face** —
both checked. The dependency is real, not aspirational.

### 5. Windows — deferred, and the plan changed: Vulkan, not CUDA

**Deferred behind the macOS release.** Measured on 20 Aug 2026, and the market
data reordered the whole thing.

Windows is 71% of desktops worldwide and 73% in Nigeria — but the torch plan
served Windows **with an NVIDIA GPU and ≥6 GB of VRAM**, which is a minority of
a minority: Steam's ~65–70% NVIDIA share is drawn from gamers, roughly a tenth
of the installed base and the tenth most likely to own a discrete card. The
Windows majority runs integrated graphics.

Then a measurement corrected an earlier mistake. "CPU is unshippable" had been
concluded from **torch fp32**, which is the worst CPU path there is. Running
dots.tts SOAR through ggml on this machine, same sentence:

| Path | RTF |
| --- | --- |
| MLX int8 · Metal — what ships | 3.3 |
| ggml q8_0 · Metal | 10 |
| ggml q8_0 · CPU | 47 |
| torch fp32 · CPU | 55–77 |

ggml on CPU is really faster than torch on CPU, but by ~1.3×, not the order of
magnitude that would change the answer. RTF 47 is eight minutes for a ten-second
note, on an Apple silicon CPU — faster than the laptops this would serve.

**Why llama.cpp's CPU story does not transfer**, which is the durable lesson:
dots.tts is not one model. It is a 1.5B Qwen backbone — the part that would be
fine on CPU — then an **18-layer diffusion transformer running 16 ODE steps per
audio patch**, then a vocoder. An LLM does one pass per token; this does sixteen
passes through an 18-layer transformer per patch. No quantisation removes a 16×
multiplier. And it generalises: nearly every zero-shot cloner is diffusion-based
for the same reason, so **cheap CPU inference and zero-shot cloning are close to
mutually exclusive today**.

**But the GPU gap is 5× on the same machine and model, and CrispASR ships
Vulkan for Windows** — 34 MB, against 722 MB for its CUDA build or ~3 GB for the
torch stack. Vulkan runs on Intel integrated graphics, AMD, and NVIDIA alike, so
it covers nearly every Windows machine that has any GPU at all. There is a
`cpu-legacy` build for pre-AVX2 machines, and the CUDA runtime ships as separate
digest-checked DLLs — the overlay pattern, already solved upstream.

That makes ggml the Windows plan and **demotes the torch pack to an NVIDIA-only
fallback**. It costs the "Fast" tier: CrispASR's dots support is SOAR only.
Windows would offer *Best quality* and nothing else — the same model as macOS,
one tier fewer, which is a far smaller break than a different model family.

**The one test that decides it**, and it needs no exotic hardware: put the 34 MB
Vulkan build on any Windows laptop with Intel graphics, point it at the SOAR
GGUF, generate one sentence, read the RTF. Apple silicon Metal gives 10, so an
Intel iGPU is bounded below by that and unknown above it. Nothing in this
section has run on Windows, and nothing should be claimed until that number
exists.

### 6. Apple silicon only — and Intel Macs are closed permanently, not just today

Checked properly on 20 Aug 2026, because "Apple silicon only" had been recorded
as an MLX consequence and it is broader than that. The **application** is not
the problem: `cargo check --target x86_64-apple-darwin` passes clean, GPUI and
all, so the shell is portable. The **runtime** has three doors and every one is
shut:

- **MLX** is Apple-silicon only by construction.
- **PyTorch**: macOS x86_64 wheels stop at **torch 2.2.2**; upstream dots.tts
  requires **torch>=2.8.0**. A hard dependency wall, not a slowness problem.
  Buzz independently confirms the ceiling — its pyproject pins `torch==2.2.2`
  for `darwin`/`x86_64` and 2.8.0 for arm64.
- **ggml/CrispASR**: its macOS release is a single `Mach-O 64-bit executable
  arm64`, not universal — five Linux x86_64 flavours and a Windows x86_64
  build, no Intel Mac one. It could be built, but it does not exist.

And the wall costs nothing worth having. Even if torch installed, an Intel Mac
is CPU-only inference — the same tier measured here at RTF 55–77, minutes of
wall clock for seconds of speech. The dependency ceiling is not denying us a
viable tier; it is saving us from shipping a bad one.

The current behaviour is already right and needs no change: the build is
**arm64-only** (`…_aarch64.dmg`), so an Intel Mac refuses it at launch with the
system's own message rather than after a download, and `host_supported()`
refuses anyway for anyone running from source. The only edit worth making is to
that refusal's wording, which blames MLX alone when the torch route is equally
closed.

### 7. Apple silicon only — **now said, before anything is downloaded**

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

### Runtime updates — **built**

The runtime no longer rides with the application. On install the bundled recipe
is staged first and unconditionally — it is the floor, and it is what a machine
with no network installs from — and then the published one is fetched from
`getlatentic/yarngo-artifacts` and used if it is newer.

Three gates decide whether a published recipe may be applied, and each is held
by a test:

- **Digest.** The lock and pyproject are checked against the SHA-256 the
  manifest names *before anything is written*, so a truncated or swapped
  download leaves the staged recipe untouched rather than half-replaced.
- **Sidecar API.** `engine.py` and the packages move together, so a recipe
  declaring an API higher than this build implements is refused. This is what
  makes remote runtime updates safe rather than a way to brick installs.
- **Minimum app version**, compared numerically — `1.10` sorts below `1.9` as
  text, which would let an old app install a recipe meant for a newer one.

Any failure is reported and stepped over: an unreachable manifest must not stop
an install the bundled recipe can finish alone. `runtime_update()` answers
whether a newer runtime exists without changing anything, so the app can offer
rather than act.

What still rides with the application is the **model catalogue**, which is
bundled and read through the same validating loader, and the sidecar itself.

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

## Versioning

**`0.1.0-alpha.1`**, and it moves forward from here — never backwards to
`0.0.1`. Two reasons, both checked rather than assumed.

A version below a release's `min_app_version` means that release is passed
over, so an app calling itself `0.0.1` would silently keep the recipe inside it
forever. And SemVer puts a pre-release *below* its release, so the floor in a
published catalogue stays at `0.1.0-alpha.1` or lower while the app is
pre-release; raise it only when a published release genuinely cannot be driven
by an older build. `scripts/tuf-repo.sh` publishes the catalogue that carries
it.

The scheme from here:

| Stage | Version | Release |
| --- | --- | --- |
| now | `0.1.0-alpha.N` | GitHub pre-release |
| feature-complete, unproven | `0.1.0-beta.N` | pre-release |
| first real release | `0.1.0` | release |

macOS is served correctly by this: `CFBundleShortVersionString` carries
`0.1.0-alpha.1` for people to read, and cargo-packager generates
`CFBundleVersion` as a timestamp — `20260820.193559` — which is what has to
increase monotonically for the system, and does so regardless of what the human
version says.

The disk image names itself `yarngo studio_0.1.0-alpha.1_aarch64.dmg`, so what
someone downloads says what it is without being opened.

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

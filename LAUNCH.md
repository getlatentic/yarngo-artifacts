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

### 1. The app is ad-hoc signed and not notarized

`scripts/package.sh` signs with `APPLE_SIGNING_IDENTITY` when set and falls back
to ad-hoc. But the `notarize()` function it defines is **never called** — there
is no `notarytool submit` and no `stapler staple` anywhere in the run. So today
the artifact is ad-hoc signed, and on anyone else's Mac Gatekeeper refuses it.

Needs: an Apple Developer account, a Developer ID Application certificate, the
notarize step actually wired into the packaging run, and a check that the
stapled artifact opens on a machine that has never seen it. The hardened runtime
and entitlements are already right — microphone, JIT, and library validation
disabled for the sidecar's native extensions.

Required for 1 and 2 both.

### 2. There is no application icon

No `icons` key in `[package.metadata.packager]`, and nothing in `packaging/`
but the plist and entitlements. The app ships with the generic binary icon.
Brand assets exist in `design/html-and-assets/project/brand/` — they need
cutting to `.icns`.

### 3. First run needs the network, and asks for a lot of it

In order: ~350 MB of CPython, then `pip install mlx-speech` and its dependency
tree, then a 3.4 GB model. The offline path covers only the first of those —
"Install from a file" takes the interpreter archive and the card says plainly
that the packages still come from the network.

This is not wrong, but it is the first impression, and it is a long one. Decide
whether launch ships as-is with the wait stated honestly (it currently is), or
whether the runtime and packages are pre-bundled into the download. Bundling
would make the download several gigabytes and cross-compiling the wheels is its
own project.

**`mlx-speech` is live on PyPI and the model repos resolve on Hugging Face** —
both checked. The dependency is real, not aspirational.

### 4. Apple Silicon only, and the app does not say so

MLX is Apple-silicon only. `runtime.rs` carries CPython target strings for
`x86_64-apple-darwin`, Linux and Windows, so the *runtime* installer looks
cross-platform while the thing it exists to run is not. On an Intel Mac the app
will install a runtime and then fail at `import mlx_speech`, with no explanation
that names the real reason.

Either gate it at launch — refuse early, in plain words, on anything but Apple
silicon — or remove the misleading targets. Minimum system version is already
declared as macOS 13.

---

## Functional gaps

### Iteration 4 is not finished

From `design/PLAN-iteration-4.md`, unbuilt and known:

- **Takes.** 4d's inspector shows `MADE WITH` and a `TAKES` list; the design
  treats several takes as belonging to one clip, and "Generate again" adds one.
  The implementation makes a separate clip each time, so the sidebar grows where
  the design's stays still. A data-model change, and the largest piece left.
- **The clip row's `more_horiz` menu** — rename, duplicate, delete. The
  inspector's own copy already refers to it. Delete currently lives at the
  bottom of the panel; duplicate does not exist.

### Settings is four-sevenths placeholder

Models and Voices are built. **General, Audio, Storage, Runtime and About** are
named panes that say "Not built yet". Two of them matter at launch:

- **About** — version, licences, the model licences, and credit for the fonts
  and the runtime. A voice-cloning app that ships without this is asking for
  trouble it does not need.
- **Storage** — the app writes gigabytes of models and clips. There is a disk
  line in the sidebar footer and per-model deletion, but no single place that
  accounts for it.

General, Audio and Runtime can stay named-but-empty if they must; the window is
already honest about it.

### No way to ship a fix

No auto-update, no version check, no crash reporting. For option 1 that is fine.
For a public download, decide between a Sparkle-style updater and simply telling
people to re-download — but decide, because the first shipped build will have
something wrong with it.

---

## Quality gates before any of it

### There are no tests. Zero.

`grep '#[test]'` across the workspace returns nothing. For an app that spawns a
Python sidecar, writes to a user's disk, and records their voice, that is the
gap I would close first. It does not need to be exhaustive; it needs to cover
the things that are painful when they break:

- `speech-engine`: the sidecar protocol — a request in, a reply parsed, an
  error surfaced. Line-delimited JSON with a process on the end of it is exactly
  where a silent format change bites.
- `runtime::install_from` against a fixture archive, and `is_installed`'s
  two-part check.
- `recorder::assess` and `envelope` — pure functions over sample buffers, cheap
  to test and load-bearing for whether a voice is accepted.
- The clip/draft state machine in `clips.rs`: start, select, rename, generate,
  and what happens to a draft when its voice is deleted.
- A round-trip through the real sidecar, run behind a feature flag, so the
  wire format is checked by something other than the app.

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

## Suggested order

1. **Tests for the sidecar protocol and the runtime installer.** Everything
   else is riskier without them.
2. **Finish iteration 4** — takes, and the row menu.
3. **About and Storage panes.**
4. **Icon, then signing and notarization**, verified on a second machine.
5. **Gate or fix the Intel path**, and the first-run story if it is changing.
6. **Privacy statement and terms**, then the download page.
7. **Decide updates** before the first build goes out, not after.

Items 1–4 are the ones that block a build being handed to anyone at all.

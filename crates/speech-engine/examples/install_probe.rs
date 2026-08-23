//! Run a real runtime install and print what it reports.
//!
//! The install path is the hardest thing here to exercise by hand: it
//! downloads hundreds of megabytes and the unit tests deliberately stop at
//! the network. The application's installer lives above this crate — in
//! yarngo-synthesis, where the store is — so this probe exercises the
//! mechanics this crate owns: recipe staged, environment built at its final
//! path, descriptor written.
//!
//! ```text
//! YARNGO_TEST_MODE=1 YARNGO_DATA=/tmp/probe cargo run -p speech-engine --example install_probe
//! ```

use speech_engine::runtime::{self, Progress};

fn main() {
    let data = speech_engine::paths::data_dir();
    let pack = runtime::pack();
    let home = data.join("runtimes").join(pack.id).join("probe");
    let mut report = |p: Progress| match p {
        Progress::Step(step) => println!("step: {step}"),
        Progress::Failed(err) => println!("FAILED: {err}"),
        Progress::Done => println!("DONE"),
        Progress::Fraction(_) => {}
    };
    let outcome = runtime::stage_bundled(pack, &home)
        .and_then(|()| runtime::build_env(&data, pack, &home, None, &mut report))
        .and_then(|()| runtime::describe_home(pack, &home));
    match outcome {
        Ok(()) => println!("DONE — {}", home.display()),
        Err(err) => println!("FAILED: {err}"),
    }
}

//! Run a real runtime install and print what it reports.
//!
//! The install path is the hardest thing here to exercise by hand: it downloads
//! hundreds of megabytes and the unit tests deliberately stop at the network.
//! This runs the whole thing against a throwaway directory, which is what the
//! Windows spike will need too.
//!
//! ```text
//! YARNGO_RUNTIME_DIR=/tmp/probe cargo run -p speech-engine --example install_probe
//! ```

use speech_engine::runtime::{self, Progress};

fn main() {
    runtime::install(|p| match p {
        Progress::Step(step) => println!("step: {step}"),
        Progress::Failed(err) => println!("FAILED: {err}"),
        Progress::Done => println!("DONE"),
        Progress::Fraction(_) => {}
    });
}

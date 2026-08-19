//! Install the speech runtime from the command line, for testing the same path
//! the first-run UI uses.
fn main() {
    println!("installing into {}", speech_engine::paths::runtime_dir().display());
    speech_engine::runtime::install(|p| match p {
        speech_engine::runtime::Progress::Step(s) => println!("  {s}"),
        speech_engine::runtime::Progress::Fraction(f) => print!("\r  {:.0}%   ", f * 100.0),
        speech_engine::runtime::Progress::Done => println!("\ndone"),
        speech_engine::runtime::Progress::Failed(e) => println!("\nFAILED: {e}"),
    });
    println!("installed: {}", speech_engine::runtime::is_installed());
}

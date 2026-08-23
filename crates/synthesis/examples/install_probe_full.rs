//! Drive the application's installer exactly as the setup screen does, and
//! then start what it installed.
fn main() {
    let places = speech_engine::paths::places();
    let store = yarngo_store::Store::open(&places.data.join("yarngo.db")).expect("store");
    let pack = speech_engine::runtime::pack();
    yarngo_synthesis::runtimes::startup(&store, &places, pack);
    let outcome = yarngo_synthesis::runtimes::install(&store, &places, pack, None, &mut |p| {
        use speech_engine::runtime::Progress;
        match p {
            Progress::Step(step) => println!("step: {step}"),
            Progress::Failed(err) => println!("FAILED: {err}"),
            Progress::Done => println!("done"),
            Progress::Fraction(_) => {}
        }
    });
    match outcome {
        Ok(ready) => {
            println!("installed {} {}", ready.descriptor.id, ready.version);
            let capabilities = ready.spawn(&places.data).probe().expect("probe again");
            println!("answers: backend={} methods={}", capabilities.backend, capabilities.methods().count());
        }
        Err(why) => println!("install failed: {why}"),
    }
}

//! What the first frame would cost if it were drawn from disk.

use std::time::Instant;

fn main() {
    let places = speech_engine::paths::places();
    let db = places.data.join("yarngo.db");
    println!("store: {}", db.display());

    let t = Instant::now();
    let store = yarngo_store::Store::open(&db).expect("open");
    println!("  Store::open              {:>7.1} ms", t.elapsed().as_secs_f64() * 1000.0);

    let t = Instant::now();
    yarngo_synthesis::runtimes::startup(&store, &places, speech_engine::runtime::pack());
    println!("  runtimes::startup        {:>7.1} ms", t.elapsed().as_secs_f64() * 1000.0);

    let t = Instant::now();
    let clips = yarngo_synthesis::library::clips(&store).expect("clips");
    println!("  library::clips ({:>2})      {:>7.1} ms", clips.len(), t.elapsed().as_secs_f64() * 1000.0);

    let t = Instant::now();
    let voices = yarngo_synthesis::library::voices(&store).expect("voices");
    println!("  library::voices ({:>2})     {:>7.1} ms", voices.len(), t.elapsed().as_secs_f64() * 1000.0);
}

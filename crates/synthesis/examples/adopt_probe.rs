fn main() {
    let places = speech_engine::paths::places();
    let store = yarngo_store::Store::open(&places.data.join("yarngo.db")).expect("store");
    yarngo_synthesis::runtimes::startup(&store, &places, speech_engine::runtime::pack());
    match yarngo_synthesis::runtimes::active(&store, &places, "mlx") {
        Some(ready) => println!(
            "answering: {} version {} via {}",
            ready.descriptor.id,
            ready.version,
            ready.descriptor.program_path().expect("program").display()
        ),
        None => println!("nothing answers"),
    }
    for row in store.installed_runtimes(None).expect("rows") {
        println!("row: {} {} ready={}", row.id, row.version, row.ready);
    }
}

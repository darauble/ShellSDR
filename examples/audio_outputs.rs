//! List the audio outputs ShellSDR would offer in the VFO Audio picker.
//! Run with `cargo run --example audio_outputs`.

fn main() {
    println!("Default  =>  (system default)");
    for o in shellsdr::engine::audio::output_options() {
        println!("{}  =>  {:?}", o.label, o.target);
    }
}

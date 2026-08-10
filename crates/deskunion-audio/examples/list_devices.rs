use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use deskunion_audio::capture::{self, Backend};

fn main() {
    let cap = capture::create(Backend::Cpal);
    for d in cap.devices().expect("list devices") {
        println!(
            "id={} name={:?} monitor={} default={}",
            d.id, d.name, d.is_monitor, d.is_default
        );
    }

    let target = std::env::args().nth(1);
    let Some(target) = target else {
        println!("\nno device id given as arg, skipping live RMS test");
        return;
    };

    println!("\ncapturing from {target:?} for 5s, printing RMS every 500ms");
    let peak_bits = Arc::new(AtomicU32::new(0));
    let peak_bits2 = peak_bits.clone();
    let mut cap = capture::create(Backend::Cpal);
    let format = cap
        .start(
            Some(&target),
            Box::new(move |data: &[f32]| {
                let rms =
                    (data.iter().map(|s| s * s).sum::<f32>() / data.len().max(1) as f32).sqrt();
                let prev = f32::from_bits(peak_bits2.load(Ordering::Relaxed));
                if rms > prev {
                    peak_bits2.store(rms.to_bits(), Ordering::Relaxed);
                }
            }),
        )
        .expect("start capture");
    println!("opened at {format:?}");

    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(500));
        let peak = f32::from_bits(peak_bits.swap(0, Ordering::Relaxed));
        println!("  peak rms this window: {peak:.5}");
    }
    cap.stop();
}

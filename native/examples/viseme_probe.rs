use prim_native::viseme::{
    frontend::Frontend,
    model::{Model, NAMES},
};
use std::{io::Write, path::Path, time::Instant};
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() >= 5,
        "usage: viseme_probe runtime pcm-f32le sample-rate streams [csv]"
    );
    let mut model = Model::load(Path::new(&args[1]))?;
    let bytes = std::fs::read(&args[2])?;
    let pcm: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let rate: u32 = args[3].parse()?;
    let count: usize = args[4].parse()?;
    let mut states = (0..count)
        .map(|_| model.state())
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut frontends: Vec<_> = (0..count).map(|_| Frontend::new(rate)).collect();
    let mut csv = if args.len() > 5 {
        Some(std::fs::File::create(&args[5])?)
    } else {
        None
    };
    if let Some(f) = csv.as_mut() {
        writeln!(f, "time,{}", NAMES.join(","))?;
    }
    let mut mel_dump = if args.len() > 6 {
        Some(std::fs::File::create(&args[6])?)
    } else {
        None
    };
    let mut times = Vec::new();
    let mut frames = 0;
    let mut peaks = [0.0_f32; 15];
    for block in pcm.chunks((rate / 100) as usize) {
        let start = Instant::now();
        for (i, (frontend, state)) in frontends.iter_mut().zip(&mut states).enumerate() {
            frontend.push(block);
            while let Some(mel) = frontend.next_mel() {
                if i == 0 {
                    if let Some(f) = mel_dump.as_mut() {
                        for x in mel {
                            f.write_all(&x.to_le_bytes())?;
                        }
                    }
                }
                let weights = model.step(state, mel)?;
                if i == 0 {
                    frames += 1;
                    for j in 0..15 {
                        peaks[j] = peaks[j].max(weights[j]);
                    }
                    if let Some(f) = csv.as_mut() {
                        writeln!(
                            f,
                            "{},{}",
                            frames as f32 / 100.0,
                            weights
                                .iter()
                                .map(|x| format!("{x:.6}"))
                                .collect::<Vec<_>>()
                                .join(",")
                        )?;
                    }
                }
            }
        }
        times.push(start.elapsed().as_micros() as u64);
    }
    times.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"sources":count,"frames":frames,"p50_us":times[times.len()/2],"p95_us":times[times.len()*95/100],"p99_us":times[times.len()*99/100],"max_us":times.last(),"peaks":peaks})
    );
    Ok(())
}

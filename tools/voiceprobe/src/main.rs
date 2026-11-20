//! Speech-to-text probe: loads one model and transcribes WAV clips with it, reporting how long the
//! model takes to load, how fast each clip is transcribed compared with its length, the most memory
//! the process used, and the text. Not part of Lithidion; see README.md.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use transcribe_cpp::{Model, RunOptions, SessionOptions};

/// The rate the models take, and the rate the watch records at.
const RATE: u32 = 16_000;

const USAGE: &str = "\
usage: voiceprobe [--threads N] [--runs N] [--language CODE] [--verbose] MODEL.gguf CLIP.wav...

  --threads N      CPU threads (default: the library's choice)
  --runs N         transcribe each clip N times and also report the fastest (default 1)
  --language CODE  language hint, such as en (default: the model's own behaviour)
  --verbose        show the speech library's own diagnostics";

struct Args {
    model: PathBuf,
    clips: Vec<PathBuf>,
    threads: i32,
    runs: u32,
    language: Option<String>,
    verbose: bool,
}

fn args() -> Result<Args> {
    let mut threads = 0;
    let mut runs = 1;
    let mut language = None;
    let mut verbose = false;
    let mut paths = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--threads" => threads = it.next().context("--threads needs a number")?.parse()?,
            "--runs" => runs = it.next().context("--runs needs a number")?.parse::<u32>()?.max(1),
            "--language" => language = Some(it.next().context("--language needs a code")?),
            "--verbose" => verbose = true,
            _ if a.starts_with("--") => bail!("unknown option {a}\n\n{USAGE}"),
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if paths.len() < 2 {
        bail!("{USAGE}");
    }
    let model = paths.remove(0);
    Ok(Args { model, clips: paths, threads, runs, language, verbose })
}

fn main() -> Result<()> {
    let args = args()?;
    if !args.verbose {
        transcribe_cpp::disable_logging();
    }
    println!("voiceprobe {}", env!("CARGO_PKG_VERSION"));
    println!("Machine: {}", machine());
    println!();

    let size = std::fs::metadata(&args.model).with_context(|| format!("cannot read {}", args.model.display()))?.len();
    let started = Instant::now();
    let model = Model::load(&args.model).with_context(|| format!("cannot load {}", args.model.display()))?;
    let session_options = SessionOptions { n_threads: args.threads, ..Default::default() };
    let mut session = model.session_with(&session_options)?;
    let load = started.elapsed();
    println!("Model:  {} ({}, {})", file_name(&args.model), model.variant(), megabytes(size));
    println!("Loaded: {} (memory now {})", seconds(load), memory("VmRSS"));
    println!();

    let options = RunOptions { language: args.language.clone(), ..Default::default() };
    let mut total_audio = Duration::ZERO;
    let mut total_first = Duration::ZERO;
    for clip in &args.clips {
        let (pcm, note) = match read(clip) {
            Ok(r) => r,
            Err(e) => {
                println!("{}: {e:#}\n", file_name(clip));
                continue;
            }
        };
        let length = Duration::from_secs_f64(pcm.len() as f64 / RATE as f64);
        let mut times = Vec::new();
        let mut text = String::new();
        let mut failed = None;
        for _ in 0..args.runs {
            let started = Instant::now();
            match session.run(&pcm, &options) {
                Ok(result) => {
                    times.push(started.elapsed());
                    text = result.text;
                }
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = failed {
            println!("{}: {} of audio{note}, not transcribed: {e}\n", file_name(clip), seconds(length));
            continue;
        }
        total_audio += length;
        total_first += times[0];
        print!("{}: {} of audio{note}, transcribed in {} ({:.2}× its length)", file_name(clip), seconds(length), seconds(times[0]), ratio(times[0], length));
        if let Some(best) = times.iter().min().filter(|_| times.len() > 1) {
            print!(", fastest {} ({:.2}×)", seconds(*best), ratio(*best, length));
        }
        println!("\n  {}\n", text.trim());
    }

    if !total_audio.is_zero() {
        println!("Overall: {} of audio in {} ({:.2}× its length, first runs)", seconds(total_audio), seconds(total_first), ratio(total_first, total_audio));
    }
    println!("Most memory used: {}", memory("VmHWM"));
    Ok(())
}

/// A clip as 16 kHz mono samples, and a note when it had to be converted.
fn read(path: &Path) -> Result<(Vec<f32>, String)> {
    let mut reader = hound::WavReader::open(path).context("not a WAV file this probe can read")?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 / scale)).collect::<Result<_, _>>()?
        }
    };
    let channels = spec.channels.max(1) as usize;
    let mono: Vec<f32> = samples.chunks(channels).map(|f| f.iter().sum::<f32>() / f.len() as f32).collect();
    let mut notes = Vec::new();
    if channels > 1 {
        notes.push(format!("{channels} channels mixed to one"));
    }
    let pcm = if spec.sample_rate == RATE {
        mono
    } else {
        notes.push(format!("resampled from {} Hz", spec.sample_rate));
        resample(&mono, spec.sample_rate)
    };
    let note = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join(", ")) };
    Ok((pcm, note))
}

/// Linear resampling to 16 kHz: enough for a probe, though a recording made at 16 kHz is better.
fn resample(input: &[f32], from: u32) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }
    let step = from as f64 / RATE as f64;
    let n = (input.len() as f64 / step) as usize;
    (0..n)
        .map(|i| {
            let x = i as f64 * step;
            let j = x as usize;
            let t = (x - j as f64) as f32;
            let a = input[j];
            let b = *input.get(j + 1).unwrap_or(&a);
            a + (b - a) * t
        })
        .collect()
}

/// The processor, its cores, and the memory, from /proc.
fn machine() -> String {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let field = |name: &str| {
        cpuinfo.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == name).then(|| v.trim().to_string())
        })
    };
    let cpu = field("model name").or_else(|| field("Hardware")).unwrap_or_else(|| std::env::consts::ARCH.into());
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    let total = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| m.lines().find_map(|l| l.strip_prefix("MemTotal:").map(|v| v.trim().to_string())))
        .and_then(|v| v.trim_end_matches(" kB").parse::<u64>().ok())
        .map(|kb| megabytes(kb * 1024))
        .unwrap_or_else(|| "unknown".into());
    format!("{cpu}, {cores} cores, {total} memory, {}", std::env::consts::ARCH)
}

/// A memory figure for this process from /proc/self/status: VmRSS now, or VmHWM at its highest.
fn memory(key: &str) -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix(key).map(|v| v.trim_start_matches(':').trim().to_string())))
        .and_then(|v| v.trim_end_matches(" kB").parse::<u64>().ok())
        .map(|kb| megabytes(kb * 1024))
        .unwrap_or_else(|| "unknown".into())
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / 1_000_000)
}

fn seconds(d: Duration) -> String {
    format!("{:.2} s", d.as_secs_f64())
}

fn ratio(took: Duration, length: Duration) -> f64 {
    took.as_secs_f64() / length.as_secs_f64().max(f64::EPSILON)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_length_and_shape() {
        let input: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        let out = resample(&input, 48_000);
        assert_eq!(out.len(), 16_000);
        assert!((out[8_000] - 0.5).abs() < 0.001);
    }
}

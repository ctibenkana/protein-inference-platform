//! Open-loop load generator for the gateway.
//!
//! Requests are sent at Poisson-distributed times at a fixed average rate,
//! whether or not earlier ones have finished, as independent users would.
//! Latency is measured from each request's *scheduled* send time, so a slow
//! system can't hide its slowness by slowing the generator down
//! ("coordinated omission").
//!
//! Sequences are random amino acids with lengths drawn from 300 reviewed
//! human proteins (lengths.txt); compute cost depends on length, not content,
//! so no real sequence is ever sent.
//!
//!   loadgen --url http://gateway/v1/embeddings --rates 2,4,8 --duration 60

use std::time::Duration;

use clap::Parser;
use serde::Serialize;
use tokio::time::{Instant, sleep_until};

const LENGTHS: &str = include_str!("lengths.txt");
const AMINO_ACIDS: &[u8] = b"ACDEFGHIKLMNPQRSTVWY";

#[derive(Parser, Debug)]
#[command(about = "Open-loop load generator for the embeddings gateway")]
struct Args {
    /// Embeddings endpoint.
    #[arg(long, default_value = "http://127.0.0.1:8080/v1/embeddings")]
    url: String,
    /// Average request rates to run, one step each, in requests per second.
    #[arg(long, value_delimiter = ',', default_value = "1,2,4")]
    rates: Vec<f64>,
    /// Measured seconds per step.
    #[arg(long, default_value_t = 60.0)]
    duration: f64,
    /// Seconds at the start of each step that are sent but not measured.
    #[arg(long, default_value_t = 10.0)]
    warmup: f64,
    /// Seconds to pause between steps so the queue empties.
    #[arg(long, default_value_t = 10.0)]
    cooldown: f64,
    /// Sequences per request.
    #[arg(long, default_value_t = 1)]
    sequences: usize,
    /// Request deadline sent as X-Request-Timeout, in seconds.
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    /// Gateway metrics URL; when set, padding and batch sizes are reported per step.
    #[arg(long)]
    metrics_url: Option<String>,
    /// Free-text label copied into the JSON output (e.g. the gateway's settings).
    #[arg(long, default_value = "")]
    label: String,
    #[arg(long, default_value_t = 42)]
    seed: u64,
}

/// SplitMix64: small, fast and good enough for load shapes; avoids a dependency.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in (0, 1].
    fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Exponential inter-arrival time for a Poisson process of `rate` per second.
    fn interarrival(&mut self, rate: f64) -> f64 {
        -self.unit().ln() / rate
    }
}

fn lengths() -> Vec<usize> {
    LENGTHS
        .lines()
        .map(|l| l.trim().parse().expect("lengths.txt holds integers"))
        .collect()
}

fn random_sequence(rng: &mut Rng, len: usize) -> String {
    (0..len)
        .map(|_| AMINO_ACIDS[rng.below(AMINO_ACIDS.len())] as char)
        .collect()
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

struct Outcome {
    /// Seconds after the step started that the request was scheduled.
    scheduled: f64,
    /// HTTP status, or 0 for a transport error or client timeout.
    status: u16,
    latency: f64,
    residues: usize,
}

#[derive(Serialize)]
struct StepReport {
    label: String,
    offered_rps: f64,
    sequences_per_request: usize,
    measured_seconds: f64,
    sent: usize,
    ok: usize,
    overloaded: usize,
    timed_out: usize,
    other_errors: usize,
    ok_rps: f64,
    residues_per_second: f64,
    p50_s: Option<f64>,
    p90_s: Option<f64>,
    p99_s: Option<f64>,
    max_s: Option<f64>,
    padding: Option<f64>,
    mean_batch_sequences: Option<f64>,
}

async fn scrape(client: &reqwest::Client, url: &Option<String>) -> Option<Vec<(String, f64)>> {
    let text = client
        .get(url.as_ref()?)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    Some(
        text.lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let (k, v) = l.rsplit_once(' ')?;
                Some((k.to_string(), v.parse().ok()?))
            })
            .collect(),
    )
}

fn metric(snapshot: &Option<Vec<(String, f64)>>, name: &str) -> Option<f64> {
    snapshot
        .as_ref()?
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| *v)
}

async fn run_step(
    args: &Args,
    client: &reqwest::Client,
    rate: f64,
    rng: &mut Rng,
    lens: &[usize],
) -> StepReport {
    let total = args.warmup + args.duration;
    let started = Instant::now();
    let mut tasks = Vec::new();
    let mut warmed_up = None;
    let mut t = rng.interarrival(rate);
    while t < total {
        let at = started + Duration::from_secs_f64(t);
        sleep_until(at).await;
        if warmed_up.is_none() && t >= args.warmup {
            warmed_up = Some(scrape(client, &args.metrics_url).await);
        }
        let seqs: Vec<String> = (0..args.sequences)
            .map(|_| {
                let len = lens[rng.below(lens.len())];
                random_sequence(rng, len)
            })
            .collect();
        let residues = seqs.iter().map(String::len).sum();
        let body = serde_json::json!({ "sequences": seqs }).to_string();
        let req = client
            .post(&args.url)
            .header("content-type", "application/json")
            .header("x-request-timeout", args.timeout.to_string())
            .timeout(Duration::from_secs_f64(args.timeout + 5.0))
            .body(body);
        let scheduled = t;
        tasks.push(tokio::spawn(async move {
            let status = match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    // Read the body: the request isn't done until it has arrived.
                    let _ = resp.bytes().await;
                    status
                }
                Err(_) => 0,
            };
            Outcome {
                scheduled,
                status,
                latency: at.elapsed().as_secs_f64(),
                residues,
            }
        }));
        t += rng.interarrival(rate);
    }
    let mut outcomes = Vec::with_capacity(tasks.len());
    for task in tasks {
        outcomes.push(task.await.expect("request task panicked"));
    }
    let after = scrape(client, &args.metrics_url).await;
    let before = warmed_up.flatten();

    let measured: Vec<&Outcome> = outcomes
        .iter()
        .filter(|o| o.scheduled >= args.warmup)
        .collect();
    let count = |f: &dyn Fn(u16) -> bool| measured.iter().filter(|o| f(o.status)).count();
    let mut ok_latency: Vec<f64> = measured
        .iter()
        .filter(|o| o.status == 200)
        .map(|o| o.latency)
        .collect();
    ok_latency.sort_by(f64::total_cmp);
    let ok_residues: usize = measured
        .iter()
        .filter(|o| o.status == 200)
        .map(|o| o.residues)
        .sum();
    let delta = |name: &str| Some(metric(&after, name)? - metric(&before, name)?);
    let padding = match (
        delta("gateway_pad_tokens_total"),
        delta("gateway_tokens_computed_total"),
    ) {
        (Some(p), Some(c)) if c > 0.0 => Some(p / c),
        _ => None,
    };
    let mean_batch = match (
        delta("gateway_batch_sequences_sum"),
        delta("gateway_batch_sequences_count"),
    ) {
        (Some(s), Some(n)) if n > 0.0 => Some(s / n),
        _ => None,
    };

    StepReport {
        label: args.label.clone(),
        offered_rps: rate,
        sequences_per_request: args.sequences,
        measured_seconds: args.duration,
        sent: measured.len(),
        ok: ok_latency.len(),
        overloaded: count(&|s| s == 503),
        timed_out: count(&|s| s == 504 || s == 0),
        other_errors: count(&|s| s != 200 && s != 503 && s != 504 && s != 0),
        ok_rps: ok_latency.len() as f64 / args.duration,
        residues_per_second: ok_residues as f64 / args.duration,
        p50_s: percentile(&ok_latency, 50.0),
        p90_s: percentile(&ok_latency, 90.0),
        p99_s: percentile(&ok_latency, 99.0),
        max_s: ok_latency.last().copied(),
        padding,
        mean_batch_sequences: mean_batch,
    }
}

fn fmt(v: Option<f64>, unit: &str) -> String {
    v.map_or("-".into(), |v| format!("{v:.2}{unit}"))
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let lens = lengths();
    let mut rng = Rng(args.seed);
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(256)
        .build()
        .expect("static client config");

    eprintln!(
        "{:>7} {:>6} {:>6} {:>5} {:>5} {:>8} {:>8} {:>8} {:>8} {:>7} {:>6}",
        "rps", "ok/s", "sent", "503", "504", "p50", "p90", "p99", "max", "padding", "batch"
    );
    for (i, &rate) in args.rates.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_secs_f64(args.cooldown)).await;
        }
        let r = run_step(&args, &client, rate, &mut rng, &lens).await;
        eprintln!(
            "{:>7.1} {:>6.2} {:>6} {:>5} {:>5} {:>8} {:>8} {:>8} {:>8} {:>7} {:>6}",
            r.offered_rps,
            r.ok_rps,
            r.sent,
            r.overloaded,
            r.timed_out + r.other_errors,
            fmt(r.p50_s, "s"),
            fmt(r.p90_s, "s"),
            fmt(r.p99_s, "s"),
            fmt(r.max_s, "s"),
            r.padding
                .map_or("-".into(), |p| format!("{:.1}%", p * 100.0)),
            fmt(r.mean_batch_sequences, ""),
        );
        println!("{}", serde_json::to_string(&r).expect("report serialises"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_match_the_sample_proteins() {
        let fasta =
            include_str!("../../../../../workloads/esm-embed/samples/uniprot_human_300.fasta");
        let from_fasta: Vec<usize> = fasta
            .split('>')
            .filter(|r| !r.trim().is_empty())
            .map(|r| r.lines().skip(1).map(str::len).sum::<usize>().min(1022))
            .collect();
        assert_eq!(lengths(), from_fasta);
    }

    #[test]
    fn interarrival_mean_matches_rate() {
        let mut rng = Rng(7);
        let n = 200_000;
        let mean = (0..n).map(|_| rng.interarrival(4.0)).sum::<f64>() / n as f64;
        assert!((mean - 0.25).abs() < 0.005, "{mean}");
    }

    #[test]
    fn sequences_use_the_alphabet_and_length() {
        let mut rng = Rng(1);
        let s = random_sequence(&mut rng, 500);
        assert_eq!(s.len(), 500);
        assert!(s.bytes().all(|b| AMINO_ACIDS.contains(&b)));
    }

    #[test]
    fn nearest_rank_percentiles() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), Some(50.0));
        assert_eq!(percentile(&v, 99.0), Some(99.0));
        assert_eq!(percentile(&v, 100.0), Some(100.0));
        assert_eq!(percentile(&[3.0], 99.0), Some(3.0));
        assert_eq!(percentile(&[], 50.0), None);
    }
}

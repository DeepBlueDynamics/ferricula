//! One task per invocation; model credentials are environment-variable names
//! in the routing TOML. The usage journal contains metadata only, never prompts.
//! Usage: task_harness models.toml usage.json daily_budget_usd < task.json
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ferricula_server::harness::{HarnessLimits, HarnessTask, RoutedBackend, run_task};
use ferricula_server::model::{ModelRouter, UsageEntry};
use ferricula_server::model_config::{ModelRoutingConfig, TaskClass};
use ferricula_server::model_transport::HttpInferenceTransport;

struct JournalLock(PathBuf);
impl Drop for JournalLock {
    fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        bail!("usage: task_harness models.toml usage.json daily_budget_usd < task.json");
    }
    let config = ModelRoutingConfig::load(&args[0])?;
    let journal = Path::new(&args[1]);
    let budget: f64 = args[2].parse().context("invalid daily budget")?;
    if !budget.is_finite() || budget < 0.0 { bail!("budget must be finite and nonnegative"); }
    let lock_path = journal.with_extension("lock");
    let mut lock = OpenOptions::new().write(true).create_new(true).open(&lock_path)
        .context("usage journal is locked; do not run concurrent workers against one journal")?;
    let _guard = JournalLock(lock_path);
    writeln!(lock, "{}", std::process::id())?;
    lock.sync_all()?;
    let previous: Vec<UsageEntry> = if journal.exists() {
        if fs::metadata(journal)?.len() > 16 * 1024 * 1024 { bail!("usage journal is too large"); }
        serde_json::from_slice(&fs::read(journal)?)?
    } else { Vec::new() };
    let router = ModelRouter::with_usage(config, previous)?;
    let limits = HarnessLimits::default();
    let mut input = String::new();
    io::stdin().take((limits.max_input_bytes + 1) as u64).read_to_string(&mut input)?;
    if input.len() > limits.max_input_bytes { bail!("task input is too large"); }
    let task: HarnessTask = serde_json::from_str(&input)?;
    let transport = HttpInferenceTransport;
    let backend = |task_class| RoutedBackend {
        router: &router, transport: &transport, task_class,
        daily_budget_usd: budget, private_context: true,
    };
    let generator = backend(TaskClass::Summarize);
    let judge = backend(TaskClass::Scan);
    let escalation = backend(TaskClass::Deliberate);
    let result = run_task(&task, &generator, &judge, Some(&escalation), &limits);

    // Persist reported charges even if a later stage failed. Hold the lock
    // until after publication; a failed save stops the worker.
    let temporary = journal.with_extension("json.tmp");
    let mut output = fs::File::create(&temporary)?;
    output.write_all(&serde_json::to_vec_pretty(&router.ledger().entries())?)?;
    output.sync_all()?;
    fs::rename(&temporary, journal)?;
    #[cfg(unix)]
    fs::File::open(journal.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")))?
        .sync_all()?;

    println!("{}", serde_json::to_string_pretty(&result?)?);
    Ok(())
}

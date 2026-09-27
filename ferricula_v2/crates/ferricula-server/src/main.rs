use std::env;

use anyhow::{Context, Result, bail};
use ferricula_server::config::RuntimeConfig;
use ferricula_server::{api, inspect_data_dir};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        None => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            println!("usage: ferricula-server inspect <data-dir> [--json]");
            println!("       ferricula-server serve --config <runtime.toml>");
        }
        Some("serve") => {
            let Some(flag) = args.next() else {
                bail!("usage: ferricula-server serve --config <runtime.toml>");
            };
            if flag != "--config" {
                bail!("expected --config, got {flag:?}");
            }
            let Some(path) = args.next() else {
                bail!("usage: ferricula-server serve --config <runtime.toml>");
            };
            let config = RuntimeConfig::load(path)?;
            let inspection = inspect_data_dir(&config.memory_dir)?;
            let bind = config.bind;
            let runtime = ferricula_server::runtime::SteveRuntime::open(config, inspection)?;
            tokio::spawn(runtime.clone().run_worker());
            tokio::spawn(runtime.clone().run_scheduler());
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("failed to bind {bind}"))?;
            eprintln!("Steve runtime listening on http://{bind}");
            axum::serve(listener, api::router(runtime)).await?;
        }
        Some("inspect") => {
            let Some(data_dir) = args.next() else {
                bail!("usage: ferricula-server inspect <data-dir> [--json]");
            };
            let json = args.next().as_deref() == Some("--json");
            let inspection = inspect_data_dir(data_dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&inspection)?);
            } else {
                println!("{} ({})", inspection.name, inspection.agent_id);
                println!("rows={} memories={}", inspection.rows, inspection.memories);
                println!(
                    "active={} forgiven={} archived={} keystones={}",
                    inspection.active,
                    inspection.forgiven,
                    inspection.archived,
                    inspection.keystones
                );
                println!(
                    "graph={} nodes/{} edges prime_tree={} terms/{} nodes/{} members",
                    inspection.graph_nodes,
                    inspection.graph_edges,
                    inspection.prime_tree_terms,
                    inspection.prime_tree_nodes,
                    inspection.prime_tree_members
                );
            }
        }
        Some(command) => bail!("unknown command {command:?}"),
    }
    Ok(())
}

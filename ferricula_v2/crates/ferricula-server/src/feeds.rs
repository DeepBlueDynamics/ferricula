use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use tokio::task::JoinSet;

const HN_API: &str = "https://hacker-news.firebaseio.com/v0";

/// Fixed-source Hacker News reader. Keeping the endpoint fixed prevents a
/// task payload from turning this capability into an SSRF/arbitrary crawler.
pub async fn read_hacker_news(limit: usize) -> Result<Vec<Value>> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let ids: Vec<u64> = client
        .get(format!("{HN_API}/topstories.json"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut tasks = JoinSet::new();
    for (position, id) in ids.into_iter().take(limit.min(50)).enumerate() {
        let client = client.clone();
        tasks.spawn(async move {
            let item: Value = client
                .get(format!("{HN_API}/item/{id}.json"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            Ok::<_, anyhow::Error>((position, item))
        });
    }

    let mut items = Vec::new();
    while let Some(result) = tasks.join_next().await {
        items.push(result??);
    }
    items.sort_by_key(|(position, _)| *position);
    Ok(items.into_iter().map(|(_, item)| item).collect())
}

//! The Pebble and Rebble app stores, merged (PROTOCOL.md section 11). Both are read without an
//! account: home pages, collections, and categories from their own APIs, search from Algolia.
//! An app in both is shown once, from the store with the newer version.

use crate::net;
use anyhow::{bail, Context, Result};
use pebble_protocol::hardware::AppPlatform;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Pebble,
    Rebble,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Pebble, Source::Rebble];

    pub fn key(self) -> &'static str {
        match self {
            Source::Pebble => "pebble",
            Source::Rebble => "rebble",
        }
    }

    pub fn from_key(key: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Source::Pebble => "Pebble App Store",
            Source::Rebble => "Rebble App Store",
        }
    }

    /// "Pebble Store", to tell two shelves of one name apart.
    pub fn short_name(self) -> &'static str {
        match self {
            Source::Pebble => "Pebble Store",
            Source::Rebble => "Rebble Store",
        }
    }

    fn api(self) -> &'static str {
        match self {
            Source::Pebble => "https://appstore-api.repebble.com/api",
            Source::Rebble => "https://appstore-api.rebble.io/api",
        }
    }

    /// Algolia's application id, search-only key, and index, as the official app uses them.
    fn search(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Source::Pebble => ("GM3S9TRYO4", "0b83b4f8e4e8e9793d2f1f93c21894aa", "apps"),
            Source::Rebble => ("7683OW76EQ", "252f4938082b8693a8a9fc0157d1d24f", "rebble-appstore-production"),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Watchface,
    Watchapp,
}

impl Kind {
    fn home(self) -> &'static str {
        match self {
            Kind::Watchface => "watchfaces",
            Kind::Watchapp => "apps",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Kind::Watchface => "watchface",
            Kind::Watchapp => "watchapp",
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub date: String,
}

/// An app as the stores describe it. Search results carry no bundle link: `pbw` is `None`
/// until the app's own page is read.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct StoreApp {
    pub source: Source,
    pub id: String,
    pub uuid: uuid::Uuid,
    pub kind: Kind,
    pub title: String,
    pub author: String,
    pub description: String,
    pub version: String,
    pub pbw: Option<String>,
    pub hearts: i64,
    pub list_image: Option<String>,
    pub screenshots: Vec<String>,
    pub changelog: Vec<Release>,
    pub website: Option<String>,
    pub source_code: Option<String>,
    pub category: String,
}

impl StoreApp {
    /// A small picture for lists: the app's icon, or a screenshot for faces.
    pub fn thumbnail(&self) -> Option<&str> {
        match self.kind {
            Kind::Watchface => self.screenshots.first().map(String::as_str).or(self.list_image.as_deref()),
            Kind::Watchapp => self.list_image.as_deref().or(self.screenshots.first().map(String::as_str)),
        }
    }
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_string()
}

fn nonempty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// An image given as a `{"144x144": url, "80x80": url}` map (store pages, preferring `size`) or
/// as a plain URL (search results).
fn image(v: Option<&Value>, size: &str) -> Option<String> {
    let v = v?;
    if let Some(url) = v.as_str() {
        return nonempty(url);
    }
    let map = v.as_object()?;
    map.get(size).and_then(Value::as_str).and_then(nonempty).or_else(|| map.values().filter_map(Value::as_str).find_map(nonempty))
}

/// Screenshots for the watch's platform when the app has them per platform (search results'
/// `asset_collections`), else its general ones.
fn screenshots(v: &Value, platform: AppPlatform) -> Vec<String> {
    let collections = v.get("asset_collections").and_then(Value::as_array);
    let per_platform = collections.and_then(|list| {
        platform.compatible().iter().find_map(|p| {
            list.iter().find(|c| c.get("hardware_platform").and_then(Value::as_str) == Some(p.codename()))?.get("screenshots")?.as_array()
        })
    });
    per_platform
        .or_else(|| v.get("screenshot_images").and_then(Value::as_array))
        .map(|list| list.iter().filter_map(|m| image(Some(m), "144x168")).collect())
        .unwrap_or_default()
}

/// Whether a watch of `platform` runs the app, from the store's compatibility table or, in
/// search results that may lack it, the platform tags.
fn runs_on(v: &Value, platform: AppPlatform) -> bool {
    let compatible = platform.compatible();
    if let Some(table) = v.get("compatibility").and_then(Value::as_object) {
        let listed = compatible.iter().any(|p| table.contains_key(p.codename()));
        if listed {
            return compatible
                .iter()
                .any(|p| table.get(p.codename()).and_then(|c| c.get("supported")).and_then(Value::as_bool) == Some(true));
        }
    }
    match v.get("_tags").and_then(Value::as_array) {
        Some(tags) => compatible.iter().any(|p| tags.iter().any(|t| t.as_str() == Some(p.codename()))),
        None => true,
    }
}

fn parse_app(source: Source, v: &Value, platform: AppPlatform) -> Option<StoreApp> {
    let kind = match v.get("type").and_then(Value::as_str)? {
        "watchface" => Kind::Watchface,
        "watchapp" => Kind::Watchapp,
        // Plugins and companion-only entries have nothing to put on the watch.
        _ => return None,
    };
    let release = v.get("latest_release");
    let version = release.map(|r| text(r, "version")).filter(|s| !s.is_empty()).unwrap_or_else(|| text(v, "version"));
    let screenshots = screenshots(v, platform);
    let changelog = v
        .get("changelog")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|c| Release { version: text(c, "version"), notes: text(c, "release_notes"), date: text(c, "published_date") })
                .collect()
        })
        .unwrap_or_default();
    Some(StoreApp {
        source,
        id: text(v, "id"),
        uuid: text(v, "uuid").parse().ok()?,
        kind,
        title: text(v, "title"),
        author: text(v, "author"),
        description: text(v, "description"),
        version,
        pbw: release.and_then(|r| nonempty(&text(r, "pbw_file"))),
        hearts: v.get("hearts").and_then(Value::as_i64).unwrap_or(0),
        list_image: image(v.get("list_image"), "144x144").or_else(|| image(v.get("icon_image"), "48x48")),
        screenshots,
        changelog,
        website: nonempty(&text(v, "website")),
        source_code: nonempty(&text(v, "source")),
        category: text(v, "category"),
    })
}

/// Compares "1.10" and "1.9" as versions, numbers part by part.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |s: &str| s.trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// One entry per UUID, keeping the newer version and else the earlier store.
fn dedupe(apps: Vec<StoreApp>) -> Vec<StoreApp> {
    let mut out: Vec<StoreApp> = Vec::new();
    for app in apps {
        match out.iter_mut().find(|a| a.uuid == app.uuid) {
            Some(existing) => {
                if newer(&app.version, &existing.version) {
                    *existing = app;
                }
            }
            None => out.push(app),
        }
    }
    out
}

async fn get(url: &str, query: &[(&str, &str)]) -> Result<Value> {
    let response =
        net::client().get(url).query(query).timeout(Duration::from_secs(30)).send().await.context("the app store cannot be reached")?;
    if !response.status().is_success() {
        bail!("the app store answered {}", response.status());
    }
    response.json().await.context("the app store's answer cannot be read")
}

fn query(platform: AppPlatform) -> Vec<(&'static str, &'static str)> {
    vec![("platform", "android"), ("hardware", platform.codename()), ("filter_hardware", "true")]
}

/// A row of a home page: a collection or a category, with its first apps.
#[derive(Debug, Clone)]
pub struct Shelf {
    pub source: Source,
    pub name: String,
    /// The API path of the full list ("/api/v1/apps/collection/top-picks/watchfaces").
    pub path: String,
    pub apps: Vec<StoreApp>,
}

#[derive(Debug, Clone, Default)]
pub struct Home {
    pub collections: Vec<Shelf>,
    pub categories: Vec<Shelf>,
}

async fn home_of(source: Source, kind: Kind, platform: AppPlatform) -> Result<Home> {
    let v = get(&format!("{}/v1/home/{}", source.api(), kind.home()), &query(platform)).await?;
    let apps: HashMap<String, StoreApp> = v
        .get("applications")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter().filter(|a| runs_on(a, platform)).filter_map(|a| parse_app(source, a, platform)).map(|a| (a.id.clone(), a)).collect()
        })
        .unwrap_or_default();
    let shelves = |key: &str| -> Vec<Shelf> {
        v.get(key)
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|c| {
                        let path = c.get("links")?.get("apps")?.as_str()?.to_string();
                        let ids = c.get("application_ids").and_then(Value::as_array).cloned().unwrap_or_default();
                        let apps = ids.iter().filter_map(Value::as_str).filter_map(|id| apps.get(id).cloned()).collect();
                        Some(Shelf { source, name: text(c, "name"), path, apps })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(Home { collections: shelves("collections"), categories: shelves("categories") })
}

/// Fails only when every store does.
fn any_ok<T>(results: Vec<Result<T>>) -> Result<Vec<T>> {
    let mut first_error = None;
    let mut out = Vec::new();
    for r in results {
        match r {
            Ok(v) => out.push(v),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if out.is_empty() => bail!("{e:#}"),
        _ => Ok(out),
    }
}

/// The chosen stores' home pages, in `Source::ALL` order; categories of the same name joined
/// (the first store's list is opened).
pub async fn home(sources: Vec<Source>, kind: Kind, platform: AppPlatform) -> Result<Home> {
    let pages = futures::future::join_all(sources.iter().map(|s| home_of(*s, kind, platform))).await;
    let mut out = Home::default();
    for h in any_ok(pages)? {
        out.collections.extend(h.collections.into_iter().filter(|s| !s.apps.is_empty()));
        // Categories are joined by name below; collections are not.
        for c in h.categories {
            if !out.categories.iter().any(|o| o.name.eq_ignore_ascii_case(&c.name)) {
                out.categories.push(c);
            }
        }
    }
    // Both stores have a "Most Loved": name the store where a name appears twice.
    let names: Vec<String> = out.collections.iter().map(|c| c.name.to_lowercase()).collect();
    for c in &mut out.collections {
        if names.iter().filter(|n| **n == c.name.to_lowercase()).count() > 1 {
            c.name = format!("{} ({})", c.name, c.source.short_name());
        }
    }
    Ok(out)
}

/// One page of a collection or category; the offset of the next page, if any.
pub async fn list(source: Source, path: &str, platform: AppPlatform, offset: usize) -> Result<(Vec<StoreApp>, Option<usize>)> {
    let base = source.api().trim_end_matches("/api");
    let url = format!("{base}{path}");
    let offset_text = offset.to_string();
    let mut q = query(platform);
    q.push(("offset", &offset_text));
    q.push(("limit", "30"));
    let v = get(&url, &q).await?;
    let apps: Vec<StoreApp> = v
        .get("data")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter(|a| runs_on(a, platform)).filter_map(|a| parse_app(source, a, platform)).collect())
        .unwrap_or_default();
    let next = v.get("links").and_then(|l| l.get("nextPage")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(|_| offset + 30);
    Ok((apps, next))
}

async fn search_in(source: Source, text_query: &str, kind: Kind, platform: AppPlatform) -> Result<Vec<StoreApp>> {
    let (app_id, key, index) = source.search();
    let body = serde_json::json!({ "query": text_query, "hitsPerPage": 40, "tagFilters": [kind.tag(), "android"] });
    let response = net::client()
        .post(format!("https://{app_id}-dsn.algolia.net/1/indexes/{index}/query"))
        .header("X-Algolia-Application-Id", app_id)
        .header("X-Algolia-API-Key", key)
        .json(&body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("the app store's search cannot be reached")?;
    if !response.status().is_success() {
        bail!("the app store's search answered {}", response.status());
    }
    let v: Value = response.json().await?;
    Ok(v.get("hits")
        .and_then(Value::as_array)
        .map(|hits| hits.iter().filter(|a| runs_on(a, platform)).filter_map(|a| parse_app(source, a, platform)).collect())
        .unwrap_or_default())
}

/// The chosen stores' search, merged.
pub async fn search(sources: Vec<Source>, text_query: String, kind: Kind, platform: AppPlatform) -> Result<Vec<StoreApp>> {
    let found = futures::future::join_all(sources.iter().map(|s| search_in(*s, &text_query, kind, platform))).await;
    Ok(dedupe(any_ok(found)?.into_iter().flatten().collect()))
}

/// An app's full page, with its bundle link.
pub async fn app(source: Source, id: String, platform: AppPlatform) -> Result<StoreApp> {
    let v = get(&format!("{}/v1/apps/id/{id}", source.api()), &query(platform)).await?;
    v.get("data")
        .and_then(Value::as_array)
        .and_then(|d| d.first())
        .and_then(|a| parse_app(source, a, platform))
        .context("the app store has no such app")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("1.10", "1.9"));
        assert!(newer("2.0", "1.99.9"));
        assert!(!newer("1.0", "1.0.0"));
        assert!(!newer("1.0", "1.1"));
    }

    #[test]
    fn parsing_and_merging() {
        let app = |version: &str, source: Source| {
            serde_json::json!({
                "id": "x", "uuid": "6e16dae9-de6a-4541-b383-df4333562cd8", "type": "watchface", "title": "NBA",
                "latest_release": { "version": version, "pbw_file": "https://x/a.pbw" },
                "screenshot_images": [{ "144x168": "https://x/s.gif" }],
                "list_image": { "144x144": "", "80x80": "" },
                "compatibility": { "emery": { "supported": true }, "basalt": { "supported": false } },
                "source": source.key(),
            })
        };
        let a = parse_app(Source::Pebble, &app("2.3.0", Source::Pebble), AppPlatform::Emery).unwrap();
        assert_eq!((a.kind, a.version.as_str(), a.pbw.as_deref()), (Kind::Watchface, "2.3.0", Some("https://x/a.pbw")));
        assert_eq!(a.thumbnail(), Some("https://x/s.gif"));
        assert_eq!(a.list_image, None);
        assert!(runs_on(&app("1", Source::Pebble), AppPlatform::Emery));
        assert!(!runs_on(&app("1", Source::Pebble), AppPlatform::Basalt));
        let b = parse_app(Source::Rebble, &app("2.4.0", Source::Rebble), AppPlatform::Emery).unwrap();
        let merged = dedupe(vec![a, b]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, Source::Rebble);
        assert!(parse_app(
            Source::Pebble,
            &serde_json::json!({"type": "plugin", "uuid": "6e16dae9-de6a-4541-b383-df4333562cd8"}),
            AppPlatform::Emery
        )
        .is_none());

        // Search hits: plain URLs, and screenshots per platform.
        let hit = serde_json::json!({
            "id": "y", "uuid": "6e16dae9-de6a-4541-b383-df4333562cd8", "type": "watchface", "version": "1.0",
            "screenshot_images": ["https://x/aplite.png"], "list_image": "https://x/icon.png",
            "asset_collections": [
                { "hardware_platform": "aplite", "screenshots": ["https://x/aplite.png"] },
                { "hardware_platform": "basalt", "screenshots": ["https://x/basalt.png"] },
            ],
        });
        let h = parse_app(Source::Pebble, &hit, AppPlatform::Emery).unwrap();
        assert_eq!(h.screenshots, ["https://x/basalt.png"]);
        assert_eq!(h.list_image.as_deref(), Some("https://x/icon.png"));
        assert_eq!(parse_app(Source::Pebble, &hit, AppPlatform::Chalk).unwrap().screenshots, ["https://x/aplite.png"]);
    }

    /// Asks both stores for real: `cargo test -- --ignored live_stores`.
    #[tokio::test]
    #[ignore]
    async fn live_stores() {
        let home = home(Source::ALL.to_vec(), Kind::Watchface, AppPlatform::Emery).await.unwrap();
        assert!(!home.collections.is_empty());
        let shelf = &home.collections[0];
        let (apps, _) = list(shelf.source, &shelf.path, AppPlatform::Emery, 0).await.unwrap();
        assert!(!apps.is_empty());
        let found = search(Source::ALL.to_vec(), "weather".into(), Kind::Watchapp, AppPlatform::Emery).await.unwrap();
        assert!(!found.is_empty());
        let full = app(found[0].source, found[0].id.clone(), AppPlatform::Emery).await.unwrap();
        assert!(full.pbw.is_some());
    }
}

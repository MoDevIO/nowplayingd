//! nowplayingd: expose this machine's MPRIS media player (read/controlled via `playerctl`)
//! as a `media_player.<name>` entity in Home Assistant.
//!
//! How it works:
//!  * State is pushed with the REST API: `POST /api/states/media_player.<name>`.
//!  * Controls: we subscribe to `call_service` events over the WebSocket API. When HA
//!    (dashboard, automation, voice…) calls e.g. `media_player.media_play_pause` on our
//!    entity, we receive the event and run the matching `playerctl` command.

use anyhow::{bail, Context, Result};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{process::Command, sync::Notify};
use tokio_tungstenite::tungstenite::Message;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "nowplayingd",
    version,
    about = "Expose this machine's media player (via playerctl) as a Home Assistant media_player",
    after_help = "Settings are read from ~/.config/nowplayingd/config.toml (or $XDG_CONFIG_HOME); \
                  command-line flags and environment variables override the file."
)]
struct Cli {
    /// Home Assistant base URL, e.g. http://homeassistant.local:8123
    #[arg(long, env = "HA_URL")]
    url: Option<String>,

    /// Home Assistant long-lived access token (Profile → Security → Long-lived access tokens)
    #[arg(long = "app-psw", alias = "app-pws", env = "HA_TOKEN", hide_env_values = true)]
    app_psw: Option<String>,

    /// Read the access token from this file instead (e.g. a sops-nix/agenix secret)
    #[arg(long, env = "HA_TOKEN_FILE", value_name = "PATH")]
    app_psw_file: Option<PathBuf>,

    /// Entity object id → media_player.<name> (default: hostname)
    #[arg(long)]
    name: Option<String>,

    /// Friendly name shown in Home Assistant (default: hostname)
    #[arg(long)]
    friendly_name: Option<String>,

    /// Only follow/control this player (passed to `playerctl --player`, e.g. "spotify,firefox")
    #[arg(long)]
    player: Option<String>,

    /// Poll interval in milliseconds (default: 1000)
    #[arg(long)]
    interval: Option<u64>,

    /// Use this config file (default: ~/.config/nowplayingd/config.toml,
    /// falling back to /etc/nowplayingd/config.toml)
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

/// Contents of config.toml – every key is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    url: Option<String>,
    #[serde(alias = "app-psw", alias = "token")]
    app_psw: Option<String>,
    #[serde(alias = "app-psw-file", alias = "token_file")]
    app_psw_file: Option<PathBuf>,
    name: Option<String>,
    #[serde(alias = "friendly-name")]
    friendly_name: Option<String>,
    player: Option<String>,
    interval: Option<u64>,
}

/// Final, merged settings (CLI/env > config file > defaults).
#[derive(Debug, Clone)]
struct Args {
    url: String,
    app_psw: String,
    name: Option<String>,
    friendly_name: Option<String>,
    player: Option<String>,
    interval: u64,
}

const SYSTEM_CONFIG: &str = "/etc/nowplayingd/config.toml";

fn user_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("nowplayingd").join("config.toml"))
}

/// User config if it exists, else the system-wide one (written by the NixOS module).
fn default_config_path() -> Option<PathBuf> {
    let user = user_config_path();
    if user.as_ref().is_some_and(|p| p.exists()) {
        return user;
    }
    let system = PathBuf::from(SYSTEM_CONFIG);
    if system.exists() {
        return Some(system);
    }
    user
}

fn read_token_file(p: &Path) -> Result<String> {
    let t = std::fs::read_to_string(p)
        .with_context(|| format!("reading token file {}", p.display()))?;
    let t = t.trim().to_string();
    if t.is_empty() {
        bail!("token file {} is empty", p.display());
    }
    Ok(t)
}

fn load_args() -> Result<Args> {
    let cli = Cli::parse();

    let (path, explicit) = match &cli.config {
        Some(p) => (Some(p.clone()), true),
        None => (default_config_path(), false),
    };
    let file = match &path {
        Some(p) if p.exists() => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("reading {}", p.display()))?;
            let cfg = toml::from_str::<FileConfig>(&text)
                .with_context(|| format!("parsing {}", p.display()))?;
            if cfg.app_psw.is_some() {
                warn_if_world_readable(p);
            }
            cfg
        }
        Some(p) if explicit => bail!("config file {} does not exist", p.display()),
        _ => FileConfig::default(),
    };

    let hint = path
        .as_ref()
        .map(|p| format!(" (pass it as a flag or set it in {})", p.display()))
        .unwrap_or_default();
    Ok(Args {
        url: cli.url.or(file.url).with_context(|| format!("missing --url{hint}"))?,
        // CLI beats file; within each, an inline token beats a token file.
        app_psw: match (cli.app_psw, cli.app_psw_file, file.app_psw, file.app_psw_file) {
            (Some(t), ..) => t,
            (None, Some(f), ..) => read_token_file(&f)?,
            (None, None, Some(t), _) => t,
            (None, None, None, Some(f)) => read_token_file(&f)?,
            _ => bail!("missing --app-psw or --app-psw-file{hint}"),
        },
        name: cli.name.or(file.name),
        friendly_name: cli.friendly_name.or(file.friendly_name),
        player: cli.player.or(file.player),
        interval: cli.interval.or(file.interval).unwrap_or(1000),
    })
}

fn warn_if_world_readable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(p) {
        if m.permissions().mode() & 0o077 != 0 {
            eprintln!(
                "warning: {} contains your token but is readable by other users – run: chmod 600 {}",
                p.display(),
                p.display()
            );
        }
    }
}

// MediaPlayerEntityFeature flags
const PAUSE: u32 = 1;
const SEEK: u32 = 2;
const VOLUME_SET: u32 = 4;
const PREVIOUS_TRACK: u32 = 16;
const NEXT_TRACK: u32 = 32;
const VOLUME_STEP: u32 = 1024;
const STOP: u32 = 4096;
const PLAY: u32 = 16384;
const SHUFFLE_SET: u32 = 32768;
const REPEAT_SET: u32 = 262144;
const FEATURES: u32 = PAUSE
    | SEEK
    | VOLUME_SET
    | PREVIOUS_TRACK
    | NEXT_TRACK
    | VOLUME_STEP
    | STOP
    | PLAY
    | SHUFFLE_SET
    | REPEAT_SET;

const HEARTBEAT: Duration = Duration::from_secs(30);
const SEP: &str = "\u{1f}"; // ASCII unit separator, won't appear in titles

struct Ctx {
    args: Args,
    entity_id: String,
    friendly_name: String,
    http: reqwest::Client,
    /// Wakes the poll loop right after we executed a command.
    poke: Notify,
    /// Forces a state push on the next poll (e.g. after HA reconnect/restart).
    force_push: AtomicBool,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct Snap {
    state: String, // playing | paused | idle
    title: String,
    artist: String,
    album: String,
    art_url: String,
    duration: Option<f64>,
    position: Option<f64>,
    volume: Option<f64>,
    shuffle: Option<bool>,
    repeat: Option<String>,
    player: String,
}

impl Snap {
    fn idle() -> Self {
        Snap { state: "idle".into(), ..Default::default() }
    }
    fn without_position(&self) -> Snap {
        Snap { position: None, ..self.clone() }
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = load_args()?;

    if Command::new("playerctl").arg("--version").output().await.is_err() {
        bail!("`playerctl` not found in PATH – install it first (e.g. `sudo apt install playerctl`)");
    }

    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "linux_pc".into());
    let object_id = sanitize(args.name.as_deref().unwrap_or(&host));
    let ctx = Arc::new(Ctx {
        entity_id: format!("media_player.{object_id}"),
        friendly_name: args.friendly_name.clone().unwrap_or(host),
        http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?,
        poke: Notify::new(),
        force_push: AtomicBool::new(true),
        args,
    });

    eprintln!("nowplayingd: exposing as {} on {}", ctx.entity_id, ctx.args.url);

    let poll = tokio::spawn(poll_loop(ctx.clone()));
    let ws = tokio::spawn(ws_loop(ctx.clone()));

    wait_for_shutdown().await;
    poll.abort();
    ws.abort();

    // Leave the entity in a sensible state instead of a stale "playing".
    let off = json!({
        "state": "off",
        "attributes": { "friendly_name": ctx.friendly_name, "supported_features": FEATURES, "icon": "mdi:laptop" }
    });
    if let Err(e) = push_state(&ctx, &off).await {
        eprintln!("could not set entity to off: {e:#}");
    }
    eprintln!("nowplayingd: stopped");
    Ok(())
}

async fn wait_for_shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

fn sanitize(s: &str) -> String {
    let s: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() { "nowplayingd".into() } else { s }
}

// ───────────────────────────── playerctl ─────────────────────────────

fn playerctl(ctx: &Ctx) -> Command {
    let mut cmd = Command::new("playerctl");
    if let Some(p) = &ctx.args.player {
        cmd.arg("--player").arg(p);
    }
    cmd.kill_on_drop(true);
    cmd
}

async fn read_player(ctx: &Ctx) -> Snap {
    let fmt = [
        "status", "title", "artist", "album", "mpris:artUrl", "mpris:length",
        "position", "volume", "shuffle", "loop", "playerName",
    ]
    .iter()
    .map(|k| format!("{{{{{k}}}}}"))
    .collect::<Vec<_>>()
    .join(SEP);

    let out = match playerctl(ctx).args(["metadata", "--format", &fmt]).output().await {
        Ok(o) if o.status.success() => o,
        _ => return Snap::idle(), // no player running / no metadata
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let f: Vec<&str> = text.trim_end_matches('\n').split(SEP).collect();
    if f.len() < 11 {
        return Snap::idle();
    }

    let micros = |s: &str| s.trim().parse::<f64>().ok().map(|v| v / 1_000_000.0);
    Snap {
        state: match f[0].trim() {
            "Playing" => "playing",
            "Paused" => "paused",
            _ => "idle",
        }
        .into(),
        title: f[1].to_string(),
        artist: f[2].to_string(),
        album: f[3].to_string(),
        art_url: f[4].to_string(),
        duration: micros(f[5]).filter(|d| *d > 0.0),
        position: micros(f[6]),
        volume: f[7].trim().parse::<f64>().ok().map(|v| (v * 100.0).round() / 100.0),
        shuffle: match f[8].trim().to_lowercase().as_str() {
            "true" | "on" => Some(true),
            "false" | "off" => Some(false),
            _ => None,
        },
        repeat: match f[9].trim() {
            "None" => Some("off".into()),
            "Track" => Some("one".into()),
            "Playlist" => Some("all".into()),
            _ => None,
        },
        player: f[10].to_string(),
    }
}

/// Map a Home Assistant media_player service call to playerctl arguments.
fn service_to_playerctl(service: &str, data: &Value) -> Option<Vec<String>> {
    let a = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect());
    match service {
        "media_play" => a(&["play"]),
        "media_pause" => a(&["pause"]),
        "media_play_pause" => a(&["play-pause"]),
        "media_stop" => a(&["stop"]),
        "media_next_track" => a(&["next"]),
        "media_previous_track" => a(&["previous"]),
        "media_seek" => {
            let p = data["seek_position"].as_f64()?;
            a(&["position", &format!("{p:.3}")])
        }
        "volume_set" => {
            let v = data["volume_level"].as_f64()?.clamp(0.0, 1.0);
            a(&["volume", &format!("{v:.3}")])
        }
        "volume_up" => a(&["volume", "0.05+"]),
        "volume_down" => a(&["volume", "0.05-"]),
        "shuffle_set" => {
            let on = data["shuffle"].as_bool()?;
            a(&["shuffle", if on { "On" } else { "Off" }])
        }
        "repeat_set" => {
            let mode = match data["repeat"].as_str()? {
                "one" => "Track",
                "all" => "Playlist",
                _ => "None",
            };
            a(&["loop", mode])
        }
        _ => None,
    }
}

// ───────────────────────────── state push ─────────────────────────────

fn ha_state(ctx: &Ctx, s: &Snap) -> Value {
    let mut at = Map::new();
    at.insert("friendly_name".into(), json!(ctx.friendly_name));
    at.insert("supported_features".into(), json!(FEATURES));
    at.insert("device_class".into(), json!("speaker"));
    at.insert("icon".into(), json!("mdi:laptop"));

    let mut put_str = |k: &str, v: &str| {
        if !v.is_empty() {
            at.insert(k.into(), json!(v));
        }
    };
    put_str("media_title", &s.title);
    put_str("media_artist", &s.artist);
    put_str("media_album_name", &s.album);
    put_str("app_name", &s.player);
    if s.art_url.starts_with("http://") || s.art_url.starts_with("https://") {
        put_str("entity_picture", &s.art_url);
    }
    if !s.title.is_empty() {
        at.insert("media_content_type".into(), json!("music"));
    }
    if let Some(d) = s.duration {
        at.insert("media_duration".into(), json!(d.round() as i64));
    }
    if let Some(p) = s.position {
        at.insert("media_position".into(), json!(p.round() as i64));
        at.insert("media_position_updated_at".into(), json!(chrono::Utc::now().to_rfc3339()));
    }
    if let Some(v) = s.volume {
        at.insert("volume_level".into(), json!(v));
        at.insert("is_volume_muted".into(), json!(v == 0.0));
    }
    if let Some(sh) = s.shuffle {
        at.insert("shuffle".into(), json!(sh));
    }
    if let Some(r) = &s.repeat {
        at.insert("repeat".into(), json!(r));
    }
    json!({ "state": s.state, "attributes": at })
}

async fn push_state(ctx: &Ctx, body: &Value) -> Result<()> {
    let url = format!("{}/api/states/{}", ctx.args.url.trim_end_matches('/'), ctx.entity_id);
    let resp = ctx
        .http
        .post(&url)
        .bearer_auth(&ctx.args.app_psw)
        .json(body)
        .send()
        .await
        .context("POST state")?;
    if !resp.status().is_success() {
        bail!("HA returned {} for {url}: {}", resp.status(), resp.text().await.unwrap_or_default());
    }
    Ok(())
}

async fn poll_loop(ctx: Arc<Ctx>) {
    let interval = Duration::from_millis(ctx.args.interval.max(100));
    let mut last: Option<(Snap, Instant)> = None;

    loop {
        let snap = read_player(&ctx).await;
        let now = Instant::now();

        let needs_push = ctx.force_push.swap(false, Ordering::SeqCst)
            || match &last {
                None => true,
                Some((prev, at)) => {
                    let elapsed = now.duration_since(*at);
                    // Position drifted from where it should be → seek or track restart.
                    let jumped = match (prev.position, snap.position) {
                        (Some(p0), Some(p1)) => {
                            let expected = if prev.state == "playing" { p0 + elapsed.as_secs_f64() } else { p0 };
                            (p1 - expected).abs() > 2.0
                        }
                        _ => false,
                    };
                    prev.without_position() != snap.without_position() || jumped || elapsed > HEARTBEAT
                }
            };

        if needs_push {
            match push_state(&ctx, &ha_state(&ctx, &snap)).await {
                Ok(()) => last = Some((snap, now)),
                Err(e) => {
                    eprintln!("state push failed: {e:#}");
                    last = None; // retry next tick
                }
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = ctx.poke.notified() => {
                // give the player a moment to apply the command before re-reading
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
    }
}

// ───────────────────────────── websocket ─────────────────────────────

fn ws_url(base: &str) -> Result<String> {
    let mut u = reqwest::Url::parse(base.trim_end_matches('/')).context("invalid --url")?;
    let scheme = match u.scheme() {
        "https" | "wss" => "wss",
        _ => "ws",
    };
    u.set_scheme(scheme).map_err(|_| anyhow::anyhow!("cannot convert URL scheme"))?;
    let path = format!("{}/api/websocket", u.path().trim_end_matches('/'));
    u.set_path(&path);
    Ok(u.to_string())
}

async fn ws_loop(ctx: Arc<Ctx>) {
    loop {
        if let Err(e) = ws_session(&ctx).await {
            eprintln!("websocket: {e:#} – reconnecting in 5s");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn ws_session(ctx: &Ctx) -> Result<()> {
    let url = ws_url(&ctx.args.url)?;
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.context("connect")?;

    // Auth handshake
    let hello = next_json(&mut ws).await?;
    if hello["type"] != "auth_required" {
        bail!("unexpected greeting: {hello}");
    }
    ws.send(Message::Text(json!({"type": "auth", "access_token": ctx.args.app_psw}).to_string()))
        .await?;
    let auth = next_json(&mut ws).await?;
    match auth["type"].as_str() {
        Some("auth_ok") => {}
        Some("auth_invalid") => bail!("authentication failed: {}", auth["message"]),
        _ => bail!("unexpected auth reply: {auth}"),
    }

    ws.send(Message::Text(
        json!({"id": 1, "type": "subscribe_events", "event_type": "call_service"}).to_string(),
    ))
    .await?;
    eprintln!("websocket: connected, listening for commands");

    // HA may have restarted (which wipes our REST-created entity) → re-publish now.
    ctx.force_push.store(true, Ordering::SeqCst);
    ctx.poke.notify_one();

    let mut next_id = 2u64;
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    ping.tick().await;

    loop {
        tokio::select! {
            msg = ws.next() => {
                let msg = match msg {
                    None => bail!("connection closed"),
                    Some(m) => m?,
                };
                match msg {
                    Message::Text(t) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                            if v["type"] == "result" && v["success"] == false {
                                eprintln!("websocket error: {}", v["error"]);
                            } else if v["type"] == "event" {
                                handle_event(ctx, &v["event"]["data"]).await;
                            }
                        }
                    }
                    Message::Close(_) => bail!("server closed connection"),
                    _ => {}
                }
            }
            _ = ping.tick() => {
                ws.send(Message::Text(json!({"id": next_id, "type": "ping"}).to_string())).await?;
                next_id += 1;
            }
        }
    }
}

async fn next_json<S>(ws: &mut S) -> Result<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match ws.next().await {
            None => bail!("connection closed during handshake"),
            Some(Ok(Message::Text(t))) => return Ok(serde_json::from_str(&t)?),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
        }
    }
}

fn targets_us(entity_field: &Value, me: &str) -> bool {
    match entity_field {
        Value::String(s) => s.split(',').any(|e| e.trim() == me),
        Value::Array(a) => a.iter().any(|e| e.as_str() == Some(me)),
        _ => false,
    }
}

async fn handle_event(ctx: &Ctx, data: &Value) {
    if data["domain"] != "media_player" {
        return;
    }
    let sd = &data["service_data"];
    if !targets_us(&sd["entity_id"], &ctx.entity_id) {
        return;
    }
    let service = data["service"].as_str().unwrap_or_default();
    let Some(cmd_args) = service_to_playerctl(service, sd) else {
        eprintln!("ignoring unsupported service media_player.{service}");
        return;
    };
    eprintln!("command: media_player.{service} → playerctl {}", cmd_args.join(" "));
    match playerctl(ctx).args(&cmd_args).output().await {
        Ok(o) if !o.status.success() => {
            eprintln!("playerctl failed: {}", String::from_utf8_lossy(&o.stderr).trim())
        }
        Err(e) => eprintln!("playerctl failed: {e}"),
        _ => {}
    }
    ctx.poke.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_urls() {
        assert_eq!(ws_url("http://ha.local:8123").unwrap(), "ws://ha.local:8123/api/websocket");
        assert_eq!(ws_url("https://ha.example.com/").unwrap(), "wss://ha.example.com/api/websocket");
    }

    #[test]
    fn service_mapping() {
        assert_eq!(service_to_playerctl("media_play_pause", &json!({})).unwrap(), vec!["play-pause"]);
        assert_eq!(
            service_to_playerctl("volume_set", &json!({"volume_level": 0.5})).unwrap(),
            vec!["volume", "0.500"]
        );
        assert_eq!(
            service_to_playerctl("repeat_set", &json!({"repeat": "all"})).unwrap(),
            vec!["loop", "Playlist"]
        );
        assert!(service_to_playerctl("turn_on", &json!({})).is_none());
    }

    #[test]
    fn targeting() {
        assert!(targets_us(&json!(["media_player.a", "media_player.me"]), "media_player.me"));
        assert!(targets_us(&json!("media_player.me"), "media_player.me"));
        assert!(!targets_us(&json!("media_player.other"), "media_player.me"));
    }

    #[test]
    fn names() {
        assert_eq!(sanitize("My-Laptop.local"), "my_laptop_local");
    }
}

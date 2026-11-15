use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A stalled audio stream is only detected when a single read times out, so a
/// shorter timeout here means a dead CDN node is abandoned faster.
const STREAM_TIMEOUT: Duration = Duration::from_secs(15);
/// How many full passes over the mirror list a download may make.
const STREAM_ROUNDS: usize = 4;
/// API requests are retried because a single DNS hiccup used to abort a whole
/// download; Bilibili's resolver answers are also cached for a short while.
const API_ATTEMPTS: usize = 3;
const DNS_TTL: Duration = Duration::from_secs(120);
/// AliDNS answers over HTTPS at this literal address, so it still works when
/// the system resolver is the thing that is broken.
const DOH_ENDPOINT: &str = "https://223.5.5.5/resolve";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";

/// Resolver with a short-lived cache and a DNS-over-HTTPS fallback.
///
/// `getaddrinfo` fails intermittently on machines with a VPN/DNS filter or an
/// overloaded ISP resolver (Windows reports `os error 11001`). Since every
/// Bilibili request needs a lookup, one bad answer used to kill the download;
/// here a failed lookup is retried, then answered by AliDNS directly.
#[derive(Default)]
struct Resolver {
    cache: Mutex<HashMap<String, (Instant, Vec<SocketAddr>)>>,
}

impl ureq::Resolver for Resolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        if let Ok(cache) = self.cache.lock() {
            if let Some((at, addrs)) = cache.get(netloc) {
                if at.elapsed() < DNS_TTL && !addrs.is_empty() {
                    return Ok(addrs.clone());
                }
            }
        }

        let addrs = system_resolve(netloc).or_else(|system_error| {
            doh_resolve(netloc).map_err(|_| system_error)
        })?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(netloc.to_owned(), (Instant::now(), addrs.clone()));
        }
        Ok(addrs)
    }
}

/// Builds an agent that either connects directly (with the resilient resolver
/// above) or through the user's proxy. Behind a proxy the host name travels in
/// `CONNECT`, so the proxy resolves DNS and the system resolver is out of the
/// loop entirely.
fn agent_builder(proxy: Option<&str>) -> ureq::AgentBuilder {
    let builder = ureq::AgentBuilder::new().resolver(Resolver::default());
    match proxy.map(str::trim).filter(|proxy| !proxy.is_empty()) {
        Some(proxy) => match ureq::Proxy::new(proxy) {
            Ok(parsed) => builder.proxy(parsed),
            Err(_) => builder,
        },
        None => builder,
    }
}

fn system_resolve(netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
    let mut last_error = None;
    for attempt in 0..2 {
        match netloc.to_socket_addrs() {
            Ok(addrs) => {
                let addrs: Vec<SocketAddr> = addrs.collect();
                if !addrs.is_empty() {
                    return Ok(addrs);
                }
            }
            Err(error) => last_error = Some(error),
        }
        if attempt == 0 {
            thread::sleep(Duration::from_millis(150));
        }
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(ErrorKind::NotFound, "DNS 没有返回任何地址")
    }))
}

fn doh_resolve(netloc: &str) -> Result<Vec<SocketAddr>> {
    let (host, port) = split_host_port(netloc);
    let url = format!("{DOH_ENDPOINT}?name={host}&type=A");
    let text = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(5))
        .build()
        .get(&url)
        .set("Accept", "application/dns-json")
        .call()
        .context("DNS over HTTPS 请求失败")?
        .into_string()?;
    let value: Value = serde_json::from_str(&text).context("DNS over HTTPS 响应无效")?;
    let addrs: Vec<SocketAddr> = value
        .get("Answer")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|answer| answer.get("type").and_then(Value::as_u64) == Some(1))
        .filter_map(|answer| answer.get("data").and_then(Value::as_str))
        .filter_map(|address| address.parse::<IpAddr>().ok())
        .map(|address| SocketAddr::new(address, port))
        .collect();
    if addrs.is_empty() {
        return Err(anyhow!("DNS over HTTPS 没有返回地址"));
    }
    Ok(addrs)
}

/// Splits `host:port` (or a bare host) into its parts; defaults to 443.
fn split_host_port(netloc: &str) -> (String, u16) {
    if let Some(rest) = netloc.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|port| port.parse().ok())
                .unwrap_or(443);
            return (host.to_owned(), port);
        }
    }
    match netloc.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|ch| ch.is_ascii_digit()) && !port.is_empty() => {
            (host.to_owned(), port.parse().unwrap_or(443))
        }
        _ => (netloc.to_owned(), 443),
    }
}

#[derive(Debug)]
pub enum DownloadEvent {
    Status(String),
    Progress(f32),
    Done(PathBuf),
    Error(String),
    Cancelled,
}

struct AudioSource {
    /// Mirrors for the same track: Bilibili hands out several CDN URLs and some
    /// of them (notably the P2P `mcdn` nodes) stall or refuse connections.
    urls: Vec<String>,
    codec: String,
    direct: bool,
}

pub fn spawn(
    input: String,
    output_dir: PathBuf,
    proxy: Option<String>,
    sender: Sender<DownloadEvent>,
    cancel: std::sync::Arc<AtomicBool>,
) {
    thread::Builder::new()
        .name("sonadial-bilibili".to_owned())
        .spawn(move || {
            let result = download(&input, &output_dir, proxy.as_deref(), &sender, &cancel);
            match result {
                Ok(path) if !cancel.load(Ordering::Relaxed) => {
                    let _ = sender.send(DownloadEvent::Done(path));
                }
                Ok(_) => {
                    let _ = sender.send(DownloadEvent::Cancelled);
                }
                Err(_) if cancel.load(Ordering::Relaxed) => {
                    let _ = sender.send(DownloadEvent::Cancelled);
                }
                Err(error) => {
                    let _ = sender.send(DownloadEvent::Error(format!("{error:#}")));
                }
            }
        })
        .ok();
}

fn download(
    input: &str,
    output_dir: &Path,
    proxy: Option<&str>,
    sender: &Sender<DownloadEvent>,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let agent = agent_builder(proxy)
        .timeout_connect(REQUEST_TIMEOUT)
        .timeout_read(REQUEST_TIMEOUT)
        .timeout_write(REQUEST_TIMEOUT)
        .build();

    let (bvid, page) = match extract_bvid(input) {
        Ok(bvid) => (bvid, extract_page(input)),
        Err(_) if input.starts_with("http://") || input.starts_with("https://") => {
            status(sender, "RESOLVING SHORT LINK");
            let response = agent
                .get(input)
                .set("User-Agent", USER_AGENT)
                .set("Referer", "https://www.bilibili.com/")
                .call()
                .context("无法解析 B 站短链接")?;
            let url = response.get_url().to_owned();
            (
                extract_bvid(&url).context("短链接中没有找到 BV 号")?,
                extract_page(&url).or_else(|| extract_page(input)),
            )
        }
        Err(error) => return Err(error),
    };

    status(sender, "RESOLVING VIDEO");
    let view_url = format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}");
    let view = fetch_json(&agent, &view_url).context("获取视频信息失败")?;
    check_api(&view, "获取视频信息失败")?;

    let data = view
        .get("data")
        .ok_or_else(|| anyhow!("视频信息缺少 data"))?;
    let (title, cid) = select_page(data, page)?;

    if cancel.load(Ordering::Relaxed) {
        return Err(anyhow!("cancelled"));
    }

    status(sender, "RESOLVING AUDIO STREAM");
    let play_url = format!(
        "https://api.bilibili.com/x/player/playurl?bvid={bvid}&cid={cid}&qn=0&fnval=16&fourk=1"
    );
    let play = fetch_json(&agent, &play_url).context("获取音频地址失败")?;
    check_api(&play, "获取音频地址失败")?;

    let source = best_audio_source(&play).ok_or_else(|| anyhow!("没有找到可下载的音轨"))?;

    fs::create_dir_all(output_dir).context("无法创建下载目录")?;
    let output = unique_output(output_dir, &safe_filename(&title), "m4a");
    let temp = std::env::temp_dir().join(format!(
        "sonadial-bilibili-{}-{}.m4a",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));

    status(sender, "DOWNLOADING");
    let result = download_stream(&source.urls, &temp, proxy, sender, cancel);
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if cancel.load(Ordering::Relaxed) {
        let _ = fs::remove_file(&temp);
        return Err(anyhow!("cancelled"));
    }

    // Bilibili's normal AAC DASH tracks are already playable M4A/MP4 files.
    // Validate one directly and move it into place, so ffmpeg is only used
    // for non-AAC streams or containers that really need fixing up.
    if source.direct {
        status(sender, "CHECKING AUDIO");
        let _ = sender.send(DownloadEvent::Progress(-1.0));
        if crate::audio::probe_track(&temp).is_ok() {
            if let Err(error) = move_into_place(&temp, &output) {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
            return Ok(output);
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&temp);
            return Err(anyhow!("cancelled"));
        }
    }

    status(sender, "CONVERTING AUDIO");
    let _ = sender.send(DownloadEvent::Progress(-1.0));
    let converted = convert_audio(&temp, &output, cancel, source.direct)
        .with_context(|| format!("{} 音轨无法直接保存，ffmpeg 兜底失败", source.codec));
    let _ = fs::remove_file(&temp);
    if let Err(error) = converted {
        let _ = fs::remove_file(&output);
        return Err(error);
    }

    if cancel.load(Ordering::Relaxed) {
        let _ = fs::remove_file(&output);
        return Err(anyhow!("cancelled"));
    }
    Ok(output)
}

fn extract_bvid(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(anyhow!("请输入 B 站视频链接或 BV 号"));
    }

    if let Some(position) = input.find("bvid=") {
        let rest = &input[position + 5..];
        let value: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric())
            .collect();
        if value.len() >= 12 && value[..2].eq_ignore_ascii_case("BV") {
            return Ok(value[..12].to_owned());
        }
    }

    if let Some(position) = input.find("BV").or_else(|| input.find("bv")) {
        let rest = &input[position + 2..];
        let suffix: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric())
            .take(10)
            .collect();
        if suffix.len() == 10 {
            return Ok(format!("BV{suffix}"));
        }
    }

    Err(anyhow!("无法从输入中提取 BV 号"))
}

/// Reads the `p` query parameter (1-based part number) from a Bilibili URL.
fn extract_page(input: &str) -> Option<u32> {
    let query = input.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("p=") {
            return value.parse::<u32>().ok().filter(|page| *page > 0);
        }
    }
    None
}

/// Whether a `part` value is a real title. Bilibili fills this field with
/// placeholders for untouched uploads: empty, `Untitled`, a raw filename such
/// as `song.mp4`, or a generated `studio_video_<timestamp>` name.
fn usable_part(part: &str) -> bool {
    if part.is_empty() || part.starts_with("studio_video_") {
        return false;
    }
    if part.eq_ignore_ascii_case("untitled") {
        return false;
    }
    let lower = part.to_ascii_lowercase();
    const VIDEO_EXTS: [&str; 9] = [
        ".mp4", ".flv", ".mkv", ".avi", ".wmv", ".mov", ".mpg", ".mpeg", ".ts",
    ];
    !VIDEO_EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// Picks the requested part of a multi-P video and returns its title and cid.
/// Multi-part videos use the part title so downloads keep the individual song
/// name; single-part uploads use the video title because their `part` field is
/// an auto-generated placeholder such as `studio_video_1726668839685`.
fn select_page(data: &Value, page: Option<u32>) -> Result<(String, u64)> {
    let video_title = data
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("bilibili_audio");
    let video_cid = data.get("cid").and_then(Value::as_u64);

    let pages = data
        .get("pages")
        .and_then(Value::as_array)
        .filter(|pages| !pages.is_empty());
    let multi_part = pages.is_some_and(|pages| pages.len() > 1);
    let selected = pages.and_then(|pages| {
        let index = page.unwrap_or(1).max(1) as usize - 1;
        pages.get(index).map(|item| (index + 1, item))
    });

    if let Some((fallback_number, item)) = selected {
        let part = item
            .get("part")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let page_number = item
            .get("page")
            .and_then(Value::as_u64)
            .unwrap_or(fallback_number as u64);
        let title = if multi_part {
            if usable_part(part) {
                part.to_owned()
            } else {
                // Keep unnamed parts distinct and sortable.
                format!("{video_title}_P{page_number:02}")
            }
        } else {
            video_title.to_owned()
        };
        if let Some(cid) = item.get("cid").and_then(Value::as_u64).or(video_cid) {
            return Ok((title, cid));
        }
    }

    let cid = video_cid.ok_or_else(|| anyhow!("视频信息缺少 cid"))?;
    Ok((video_title.to_owned(), cid))
}

fn check_api(value: &Value, context: &str) -> Result<()> {
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(anyhow!("{context}: {message} ({code})"));
    }
    Ok(())
}

/// GETs a JSON document, retrying transient network and body failures.
/// Bilibili API errors (`code != 0`) are not retried: those are answers, not
/// transport problems.
fn fetch_json(agent: &ureq::Agent, url: &str) -> Result<Value> {
    let mut last_error = None;
    for attempt in 0..API_ATTEMPTS {
        let result = agent
            .get(url)
            .set("User-Agent", USER_AGENT)
            .set("Referer", "https://www.bilibili.com/")
            .call()
            .map_err(|error| anyhow!("{error}"))
            .and_then(|response| response.into_string().map_err(|error| anyhow!("{error}")))
            .and_then(|text| serde_json::from_str(&text).map_err(|error| anyhow!("{error}")));
        match result {
            Ok(value) => return Ok(value),
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < API_ATTEMPTS {
                    thread::sleep(Duration::from_millis(250 * (attempt as u64 + 1)));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("请求失败")))
}

fn best_audio_source(play: &Value) -> Option<AudioSource> {
    let data = play.get("data")?;

    if let Some(audio) = data
        .get("dash")
        .and_then(|dash| dash.get("audio"))
        .and_then(Value::as_array)
    {
        let mut best_aac: Option<(&Value, u64)> = None;
        let mut best_any: Option<(&Value, u64)> = None;
        for item in audio {
            let bandwidth = item.get("bandwidth").and_then(Value::as_u64).unwrap_or(0);
            if best_any.map_or(true, |(_, best)| bandwidth > best) {
                best_any = Some((item, bandwidth));
            }
            let codec = item
                .get("codecs")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let codecid = item.get("codecid").and_then(Value::as_u64);
            let is_aac = codec.starts_with("mp4a") || (codec.is_empty() && codecid == Some(0));
            if is_aac && best_aac.map_or(true, |(_, best)| bandwidth > best) {
                best_aac = Some((item, bandwidth));
            }
        }

        if let Some((item, _)) = best_aac {
            return audio_source(item, true);
        }
        if let Some((item, _)) = best_any {
            return audio_source(item, false);
        }
    }

    data.get("durl")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("url"))
        .and_then(Value::as_str)
        .map(|url| AudioSource {
            urls: vec![url.to_owned()],
            codec: "durl".to_owned(),
            direct: false,
        })
}

/// Whether a URL points at a Bilibili P2P/edge node. These hosts answer the
/// request and then often stop sending data, so plain CDN mirrors are tried
/// first and P2P only as a last resort.
fn is_p2p_mirror(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host.contains("mcdn") || host.contains("szbdyd") || host.contains("p2p")
}

fn audio_source(item: &Value, direct: bool) -> Option<AudioSource> {
    let mut urls: Vec<String> = Vec::new();
    let base = item
        .get("baseUrl")
        .or_else(|| item.get("base_url"))
        .and_then(Value::as_str);
    for url in base.into_iter().chain(
        ["backupUrl", "backup_url"]
            .iter()
            .filter_map(|key| item.get(*key))
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(Value::as_str),
    ) {
        if !url.is_empty() && !urls.iter().any(|seen| seen == url) {
            urls.push(url.to_owned());
        }
    }
    if urls.is_empty() {
        return None;
    }
    // Stable sort: keeps Bilibili's own ordering among plain CDN mirrors.
    urls.sort_by_key(|url| is_p2p_mirror(url));

    let codec = item
        .get("codecs")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    Some(AudioSource {
        urls,
        codec,
        direct,
    })
}

fn move_into_place(from: &Path, to: &Path) -> Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).context("无法保存音频文件")?;
    let _ = fs::remove_file(from);
    Ok(())
}

fn download_stream(
    urls: &[String],
    temp: &Path,
    proxy: Option<&str>,
    sender: &Sender<DownloadEvent>,
    cancel: &AtomicBool,
) -> Result<()> {
    let stream_agent = agent_builder(proxy)
        .timeout_connect(REQUEST_TIMEOUT)
        .timeout_read(STREAM_TIMEOUT)
        .timeout_write(STREAM_TIMEOUT)
        .build();

    let mut downloaded = 0_u64;
    let mut total = 0_u64;
    let mut last_error: Option<anyhow::Error> = None;

    // A CDN node that stops sending mid-transfer is retried from the byte we
    // already have, so a dead mirror only costs the time it takes to notice.
    for _ in 0..STREAM_ROUNDS {
        for url in urls {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("cancelled"));
            }
            match fetch_into(
                &stream_agent,
                url,
                temp,
                &mut downloaded,
                &mut total,
                sender,
                cancel,
            ) {
                Ok(true) => {
                    if downloaded == 0 {
                        return Err(anyhow!("音频流为空"));
                    }
                    return Ok(());
                }
                Ok(false) => {
                    last_error = Some(anyhow!(
                        "镜像返回的数据不完整（{downloaded}/{total} 字节）"
                    ));
                }
                Err(error) if cancel.load(Ordering::Relaxed) => return Err(error),
                Err(error) => last_error = Some(error),
            }
            let _ = sender.send(DownloadEvent::Progress(-1.0));
        }
    }

    if downloaded == 0 {
        return Err(anyhow!("音频流为空"));
    }
    let detail = last_error
        .map(|error| format!("{error:#}"))
        .unwrap_or_else(|| "未知错误".to_owned());
    Err(anyhow!(
        "音频流读取失败，已尝试 {} 个镜像（已下载 {downloaded} 字节）: {detail}",
        urls.len()
    ))
}

/// Fetches `url` into `temp`, resuming at `*downloaded` when possible.
/// Returns `true` once the whole track has been written.
fn fetch_into(
    agent: &ureq::Agent,
    url: &str,
    temp: &Path,
    downloaded: &mut u64,
    total: &mut u64,
    sender: &Sender<DownloadEvent>,
    cancel: &AtomicBool,
) -> Result<bool> {
    let mut request = agent
        .get(url)
        .set("User-Agent", USER_AGENT)
        .set("Referer", "https://www.bilibili.com/");
    if *downloaded > 0 {
        request = request.set("Range", &format!("bytes={}-", *downloaded));
    }
    let response = request.call().context("下载音频流失败")?;

    let length = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    // A mirror that ignores `Range` answers 200 with the full body, in which
    // case the partial file has to be thrown away.
    let resuming = *downloaded > 0 && response.status() == 206 && length > 0;
    if resuming {
        *total = *downloaded + length;
    } else {
        *downloaded = 0;
        *total = length;
    }
    if *total == 0 {
        let _ = sender.send(DownloadEvent::Progress(-1.0));
    }

    let mut reader = response.into_reader();
    let mut file = if resuming {
        OpenOptions::new()
            .append(true)
            .open(temp)
            .context("无法打开临时音频文件")?
    } else {
        File::create(temp).context("无法创建临时音频文件")?
    };
    let mut buffer = [0_u8; 64 * 1024];
    let mut last_percent = -1_i32;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("cancelled"));
        }
        let read = reader.read(&mut buffer).context("音频流读取失败")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .context("写入临时音频文件失败")?;
        *downloaded += read as u64;
        if *total > 0 {
            let percent = ((*downloaded).saturating_mul(100) / *total).min(100) as i32;
            if percent != last_percent {
                last_percent = percent;
                let _ = sender.send(DownloadEvent::Progress(percent as f32 / 100.0));
            }
        }
    }
    file.flush().ok();

    Ok(*total == 0 || *downloaded >= *total)
}

fn convert_audio(input: &Path, output: &Path, cancel: &AtomicBool, allow_copy: bool) -> Result<()> {
    let input_text = input.to_string_lossy();
    let output_text = output.to_string_lossy();
    if allow_copy {
        let first = run_ffmpeg(
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-i",
                input_text.as_ref(),
                "-map",
                "0:a:0",
                "-vn",
                "-c:a",
                "copy",
                "-y",
                output_text.as_ref(),
            ],
            cancel,
        );
        if first.is_ok() && output.is_file() && crate::audio::probe_track(output).is_ok() {
            return Ok(());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("cancelled"));
        }
        let _ = fs::remove_file(output);
    }

    run_ffmpeg(
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-i",
            input_text.as_ref(),
            "-map",
            "0:a:0",
            "-vn",
            "-c:a",
            "aac",
            "-b:a",
            "256k",
            "-y",
            output_text.as_ref(),
        ],
        cancel,
    )
    .context("ffmpeg 转换失败，请确认 ffmpeg 在 PATH 中")?;

    if output.is_file() && crate::audio::probe_track(output).is_ok() {
        Ok(())
    } else {
        Err(anyhow!("ffmpeg 没有生成可播放的音频文件"))
    }
}

fn run_ffmpeg(args: &[&str], cancel: &AtomicBool) -> Result<()> {
    let mut command = Command::new("ffmpeg");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().context("无法启动 ffmpeg")?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!("cancelled"));
        }
        match child.try_wait().context("等待 ffmpeg 失败")? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => return Err(anyhow!("ffmpeg 退出状态 {status}")),
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn safe_filename(title: &str) -> String {
    let mut result = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_control() || matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            result.push('_');
        } else {
            result.push(ch);
        }
    }
    let result = result
        .trim()
        .trim_end_matches(|ch| ch == '.' || ch == ' ')
        .to_owned();
    let result: String = result.chars().take(80).collect();
    if result.is_empty() {
        "bilibili_audio".to_owned()
    } else {
        result
    }
}

fn unique_output(directory: &Path, stem: &str, extension: &str) -> PathBuf {
    let first = directory.join(format!("{stem}.{extension}"));
    if !first.exists() {
        return first;
    }
    for index in 2..10_000 {
        let candidate = directory.join(format!("{stem} ({index}).{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join(format!(
        "{stem}-{}.{extension}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ))
}

fn status(sender: &Sender<DownloadEvent>, text: &str) {
    let _ = sender.send(DownloadEvent::Status(text.to_owned()));
}

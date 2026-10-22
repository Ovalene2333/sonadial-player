use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";

#[derive(Debug)]
pub enum DownloadEvent {
    Status(String),
    Progress(f32),
    Done(PathBuf),
    Error(String),
    Cancelled,
}

struct AudioSource {
    url: String,
    codec: String,
    direct: bool,
}

pub fn spawn(
    input: String,
    output_dir: PathBuf,
    sender: Sender<DownloadEvent>,
    cancel: std::sync::Arc<AtomicBool>,
) {
    thread::Builder::new()
        .name("sonadial-bilibili".to_owned())
        .spawn(move || {
            let result = download(&input, &output_dir, &sender, &cancel);
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
    sender: &Sender<DownloadEvent>,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let agent = ureq::AgentBuilder::new()
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
    let view: Value = serde_json::from_str(
        &agent
            .get(&view_url)
            .set("User-Agent", USER_AGENT)
            .set("Referer", "https://www.bilibili.com/")
            .call()
            .context("获取视频信息失败")?
            .into_string()?,
    )
    .context("视频信息不是有效 JSON")?;
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
    let play: Value = serde_json::from_str(
        &agent
            .get(&play_url)
            .set("User-Agent", USER_AGENT)
            .set("Referer", "https://www.bilibili.com/")
            .call()
            .context("获取音频地址失败")?
            .into_string()?,
    )
    .context("音频地址不是有效 JSON")?;
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
    let result = download_stream(&agent, &source.url, &temp, sender, cancel);
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
            url: url.to_owned(),
            codec: "durl".to_owned(),
            direct: false,
        })
}

fn audio_source(item: &Value, direct: bool) -> Option<AudioSource> {
    let url = item
        .get("baseUrl")
        .or_else(|| item.get("base_url"))
        .and_then(Value::as_str)?;
    let codec = item
        .get("codecs")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    Some(AudioSource {
        url: url.to_owned(),
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
    agent: &ureq::Agent,
    url: &str,
    temp: &Path,
    sender: &Sender<DownloadEvent>,
    cancel: &AtomicBool,
) -> Result<()> {
    let response = agent
        .get(url)
        .set("User-Agent", USER_AGENT)
        .set("Referer", "https://www.bilibili.com/")
        .call()
        .context("下载音频流失败")?;
    let total = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if total == 0 {
        let _ = sender.send(DownloadEvent::Progress(-1.0));
    }

    let mut reader = response.into_reader();
    let mut file = File::create(temp).context("无法创建临时音频文件")?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut downloaded = 0_u64;
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
        downloaded += read as u64;
        if total > 0 {
            let percent = ((downloaded.saturating_mul(100) / total).min(100)) as i32;
            if percent != last_percent {
                last_percent = percent;
                let _ = sender.send(DownloadEvent::Progress(percent as f32 / 100.0));
            }
        }
    }
    file.flush().ok();

    if downloaded == 0 {
        return Err(anyhow!("音频流为空"));
    }
    Ok(())
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

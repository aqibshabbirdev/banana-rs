//! Google Photos import through the Google Photos Picker API. Since March 2025 this is the only
//! way an app may read someone's Google Photos: they sign in, pick photos in Google's own
//! picker page, and the app downloads just those.
//!
//! Needs an OAuth client of type "Desktop app" from the user's own Google Cloud project (the
//! Photos Picker API enabled), saved in the app. Sign-in uses the installed-app flow: the
//! browser opens Google's consent page, Google redirects to a one-off listener on 127.0.0.1,
//! and the code is exchanged with PKCE. The access token lives only in memory for the import.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, NaiveDateTime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{save_file, Cancel, Filter, Msg, Reporter};

const SCOPE: &str = "https://www.googleapis.com/auth/photospicker.mediaitems.readonly";
const PICKER: &str = "https://photospicker.googleapis.com/v1";

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl ClientConfig {
    fn file() -> Option<PathBuf> {
        Some(crate::store::data_dir()?.join("google_client.json"))
    }

    pub fn load() -> Self {
        Self::file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(f) = Self::file() {
            let _ = std::fs::create_dir_all(f.parent().unwrap());
            let _ = std::fs::write(f, serde_json::to_vec_pretty(self).unwrap());
        }
    }

    pub fn is_set(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
}

pub const SETUP_STEPS: &str = "1. Open console.cloud.google.com and create a project (free).\n\
2. APIs & Services → Library → enable \"Google Photos Picker API\".\n\
3. OAuth consent screen → External → add yourself as a test user.\n\
4. Credentials → Create credentials → OAuth client ID → Desktop app.\n\
5. Paste the client ID and client secret here.";

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..chunk.len() + 1 {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00"), 16).unwrap_or(0));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn random_token(len: usize) -> String {
    let mut b = vec![0u8; len];
    getrandom::getrandom(&mut b).expect("random");
    base64url(&b)
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Waits for Google's redirect to the local listener and returns the authorization code.
fn wait_for_code(listener: &TcpListener, state: &str, cancel: &Cancel) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if cancel.cancelled() {
            return Err("Stopped.".into());
        }
        if Instant::now() > deadline {
            return Err("Sign-in timed out.".into());
        }
        let (mut stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let query = request.split_whitespace().nth(1).and_then(|p| p.split_once('?')).map(|(_, q)| q.to_string()).unwrap_or_default();
        let param = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).map(decode);
        let body = "<html><body style='font-family:sans-serif;background:#1b1b1c;color:#e3e3e3;text-align:center;padding-top:80px'>\
            <h2>Signed in</h2><p>You can close this tab and go back to Banana Photos.</p></body></html>";
        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        if param("state").as_deref() != Some(state) {
            continue; // e.g. a favicon request, or someone else's redirect
        }
        if let Some(err) = param("error") {
            return Err(format!("Google sign-in was not completed ({err})."));
        }
        if let Some(code) = param("code") {
            return Ok(code);
        }
    }
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
}

#[derive(Deserialize)]
struct Session {
    id: String,
    #[serde(rename = "pickerUri")]
    picker_uri: String,
    #[serde(rename = "mediaItemsSet", default)]
    media_items_set: bool,
    #[serde(rename = "pollingConfig")]
    polling: Option<Polling>,
}

#[derive(Deserialize)]
struct Polling {
    #[serde(rename = "pollInterval")]
    poll_interval: Option<String>,
}

#[derive(Deserialize)]
struct ItemsPage {
    #[serde(rename = "mediaItems", default)]
    media_items: Vec<PickedItem>,
    #[serde(rename = "nextPageToken")]
    next: Option<String>,
}

#[derive(Deserialize)]
struct PickedItem {
    #[serde(rename = "createTime")]
    create_time: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "mediaFile")]
    file: MediaFile,
}

#[derive(Deserialize)]
struct MediaFile {
    #[serde(rename = "baseUrl")]
    base_url: String,
    filename: Option<String>,
}

fn http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            format!("Google said {code}: {}", body.chars().take(300).collect::<String>())
        }
        other => other.to_string(),
    }
}

fn run(cfg: &ClientConfig, dest: &Path, filter: &Filter, cancel: &Cancel, r: &Reporter) -> Result<String, String> {
    // 1. Sign in.
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr().map_err(|e| e.to_string())?.port());
    let verifier = random_token(48);
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        encode(cfg.client_id.trim()),
        encode(&redirect),
        encode(SCOPE),
    );
    r.status("Sign in to Google in your browser…");
    open_browser(&url);
    let code = wait_for_code(&listener, &state, cancel)?;
    drop(listener);

    let token: Token = ureq::post("https://oauth2.googleapis.com/token")
        .send_form(&[
            ("code", code.as_str()),
            ("client_id", cfg.client_id.trim()),
            ("client_secret", cfg.client_secret.trim()),
            ("redirect_uri", redirect.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .map_err(http_err)?
        .into_json()
        .map_err(|e| e.to_string())?;
    let auth = format!("Bearer {}", token.access_token);

    // 2. Let them pick in Google's picker.
    let session: Session = ureq::post(&format!("{PICKER}/sessions"))
        .set("Authorization", &auth)
        .send_json(serde_json::json!({}))
        .map_err(http_err)?
        .into_json()
        .map_err(|e| e.to_string())?;
    r.status("Pick photos in the browser, then press Done there…");
    open_browser(&format!("{}/autoclose", session.picker_uri.trim_end_matches('/')));
    let interval = session
        .polling
        .and_then(|p| p.poll_interval)
        .and_then(|s| s.trim_end_matches('s').parse::<f64>().ok())
        .unwrap_or(3.0)
        .clamp(1.0, 30.0);
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    loop {
        if cancel.cancelled() {
            return Err("Stopped.".into());
        }
        if Instant::now() > deadline {
            return Err("Picking timed out.".into());
        }
        std::thread::sleep(Duration::from_secs_f64(interval));
        let s: Session = ureq::get(&format!("{PICKER}/sessions/{}", session.id))
            .set("Authorization", &auth)
            .call()
            .map_err(http_err)?
            .into_json()
            .map_err(|e| e.to_string())?;
        if s.media_items_set {
            break;
        }
    }

    // 3. List what was picked, filter, download.
    let mut items = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut req = ureq::get(&format!("{PICKER}/mediaItems")).set("Authorization", &auth).query("sessionId", &session.id).query("pageSize", "100");
        if let Some(p) = &page {
            req = req.query("pageToken", p);
        }
        let got: ItemsPage = req.call().map_err(http_err)?.into_json().map_err(|e| e.to_string())?;
        items.extend(got.media_items);
        match got.next {
            Some(n) if !n.is_empty() => page = Some(n),
            _ => break,
        }
    }
    let picked: Vec<(PickedItem, NaiveDateTime, bool, String)> = items
        .into_iter()
        .map(|it| {
            let taken = it
                .create_time
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Local).naive_local())
                .unwrap_or_else(super::now);
            let is_video = it.kind.as_deref() == Some("VIDEO");
            let name = it.file.filename.clone().unwrap_or_else(|| format!("google_{}.{}", taken.format("%Y%m%d_%H%M%S"), if is_video { "mp4" } else { "jpg" }));
            (it, taken, is_video, name)
        })
        .filter(|(_, taken, is_video, name)| filter.matches(name, *taken, *is_video))
        .collect();

    let total = picked.len();
    let (mut copied, mut skipped, mut failed) = (0, 0, 0);
    for (i, (it, taken, is_video, name)) in picked.iter().enumerate() {
        if cancel.cancelled() {
            break;
        }
        // "=d" is the original photo with its metadata, "=dv" the video.
        let url = format!("{}={}", it.file.base_url, if *is_video { "dv" } else { "d" });
        let result = (|| {
            let resp = ureq::get(&url).set("Authorization", &auth).call().map_err(http_err)?;
            let tmp = dest.join(format!(".banana-download-{}", random_token(6)));
            std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
            let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            std::io::copy(&mut resp.into_reader(), &mut f).map_err(|e| e.to_string())?;
            drop(f);
            save_file(dest, name, *taken, &tmp).map_err(|e| e.to_string())
        })();
        match result {
            Ok(true) => copied += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                failed += 1;
                r.send(Msg::Failed(format!("{name}: {e}")));
            }
        }
        r.send(Msg::Progress { done: i + 1, total });
    }
    let _ = ureq::delete(&format!("{PICKER}/sessions/{}", session.id)).set("Authorization", &auth).call();
    Ok(super::summary_for("Imported", copied, skipped, failed, cancel.cancelled()))
}

pub fn spawn_picker_import(cfg: ClientConfig, dest: PathBuf, filter: Filter, cancel: Cancel, r: Reporter) {
    std::thread::spawn(move || match run(&cfg, &dest, &filter, &cancel, &r) {
        Ok(summary) => r.send(Msg::Done(summary)),
        Err(e) => r.send(Msg::Done(format!("Google Photos import didn't finish: {e}"))),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(base64url(&Sha256::digest(verifier.as_bytes())), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn url_encoding_round_trips() {
        let s = "http://127.0.0.1:5000/?a=b c&d=é";
        assert_eq!(decode(&encode(s)), s);
    }
}

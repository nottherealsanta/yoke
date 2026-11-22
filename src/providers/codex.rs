//! ChatGPT (Codex) sign-in: browser PKCE or device code.
//!
//! The Responses-dialect provider itself is M7. This file owns the OAuth flow,
//! the hand-rolled encoding it needs, and the token exchange shared with
//! `auth::CodexAuth`'s refresh.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use super::responses::{self, ResponsesQuirks};
use crate::auth::{self, CodexTokens};

pub const ISSUER: &str = "https://auth.openai.com";
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const SCOPES: &str = "openid profile email offline_access";
/// The device page that accepts the code; `/deviceauth` is the API root.
pub const DEVICE_URL: &str = "https://auth.openai.com/codex/device";

pub const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const DEFAULT_MODEL: &str = "gpt-6.1-terra";

/// Codex replays encrypted reasoning, uses the conversation id as a prompt
/// cache key, and sends it again in the `session_id` header.
pub fn quirks() -> ResponsesQuirks {
    ResponsesQuirks {
        encrypted_reasoning: true,
        prompt_cache_key: true,
        session_header: Some("session_id"),
    }
}

pub fn new(home: &Path) -> responses::ResponsesProvider {
    responses::ResponsesProvider::new(
        "codex",
        ENDPOINT,
        Box::new(auth::CodexAuth::new(home)),
        quirks(),
    )
}

const LISTEN: &str = "127.0.0.1:1455";
const BROWSER_WAIT: Duration = Duration::from_secs(300);
const DEVICE_WAIT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Browser,
    Device,
}

/// Sign in and store the tokens. Messages for the user (the URL or code) go
/// through `notify`; tokens never do.
pub async fn login(home: &Path, method: Method, notify: impl Fn(&str)) -> Result<()> {
    match method {
        Method::Browser => browser_login(home, &notify).await,
        Method::Device => device_login(home, &notify).await,
    }
}

async fn browser_login(home: &Path, notify: &impl Fn(&str)) -> Result<()> {
    let verifier = b64url(&random_bytes(48)?);
    let challenge = b64url(&sha256(verifier.as_bytes()));
    let state = b64url(&random_bytes(32)?);

    let listener = TcpListener::bind(LISTEN).await.with_context(|| {
        format!("codex: cannot listen on {LISTEN}; use `yoke login codex --device`")
    })?;
    let url = authorize_url(&challenge, &state);
    notify(&format!("Open {url} to sign in to ChatGPT."));
    open_browser(&url);

    let code = match timeout(BROWSER_WAIT, wait_callback(&listener, &state)).await {
        Ok(result) => result?,
        Err(_) => bail!("codex: browser login timed out"),
    };
    let response = token_request(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("code_verifier", &verifier),
        ("redirect_uri", REDIRECT_URI),
    ])
    .await?;
    persist(home, &response)
}

async fn wait_callback(listener: &TcpListener, state: &str) -> Result<String> {
    loop {
        let (mut socket, _) = listener.accept().await?;
        let mut buffer = [0_u8; 4096];
        let read = timeout(Duration::from_secs(5), socket.read(&mut buffer)).await;
        let Ok(Ok(read)) = read else {
            continue;
        };
        let request = String::from_utf8_lossy(&buffer[..read]);
        let line = request.lines().next().unwrap_or_default();
        match callback_request(line, state) {
            None => respond(&mut socket, "404 Not Found", "Not found.\n").await?,
            Some(Ok(code)) => {
                respond(
                    &mut socket,
                    "200 OK",
                    "<html><body><p>Login complete. Return to Yoke.</p></body></html>\n",
                )
                .await?;
                return Ok(code);
            }
            Some(Err(error)) => {
                respond(
                    &mut socket,
                    "400 Bad Request",
                    "<html><body><p>Login failed. Return to Yoke.</p></body></html>\n",
                )
                .await?;
                return Err(error);
            }
        }
    }
}

async fn respond(socket: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await?;
    socket.shutdown().await
}

/// The URL that opens ChatGPT's sign-in page.
pub fn authorize_url(challenge: &str, state: &str) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "yoke"),
    ];
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{ISSUER}/oauth/authorize?{query}")
}

/// Parse one HTTP request line. `None` means it is not the callback path.
pub fn callback_request(line: &str, expected_state: &str) -> Option<Result<String>> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/auth/callback" {
        return None;
    }
    Some(callback_code(query, expected_state))
}

fn callback_code(query: &str, expected_state: &str) -> Result<String> {
    let params: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect();
    let get = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or("")
    };

    let error = get("error");
    if !error.is_empty() {
        bail!("codex: login was denied ({error})");
    }
    if !constant_time_eq(get("state"), expected_state) {
        bail!("codex: login was denied or invalid");
    }
    let code = get("code");
    if code.is_empty() {
        bail!("codex: login callback had no code");
    }
    Ok(code.to_string())
}

async fn device_login(home: &Path, notify: &impl Fn(&str)) -> Result<()> {
    let (status, data) = post_json(
        &format!("{ISSUER}/api/accounts/deviceauth/usercode"),
        json!({"client_id": CLIENT_ID}),
    )
    .await?;
    if status >= 400 {
        bail!("codex: could not start device login (HTTP {status})");
    }
    let user_code = data["user_code"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("codex: device response had no user code"))?;
    let device_id = data["device_auth_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("codex: device response had no auth id"))?;
    notify(&format!("Open {DEVICE_URL} and enter code {user_code}"));
    let interval = data["interval"].as_f64().unwrap_or(5.0).max(1.0);

    let started = std::time::Instant::now();
    while started.elapsed() < DEVICE_WAIT {
        // Ask less often than the server allows, so polling never trips
        // `slow_down`.
        tokio::time::sleep(Duration::from_secs_f64(interval + 3.0)).await;
        let (status, poll) = post_json(
            &format!("{ISSUER}/api/accounts/deviceauth/token"),
            json!({"device_auth_id": device_id, "user_code": user_code}),
        )
        .await?;
        if status == 403 || status == 404 {
            continue;
        }
        if status >= 400 {
            bail!("codex: device login failed (HTTP {status})");
        }
        let code = poll["authorization_code"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("codex: device response had no authorization code"))?;
        let verifier = poll["code_verifier"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("codex: device response had no code verifier"))?;
        let redirect = format!("{ISSUER}/deviceauth/callback");
        let response = token_request(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", &redirect),
        ])
        .await?;
        return persist(home, &response);
    }
    bail!("codex: device login timed out")
}

fn persist(home: &Path, response: &Value) -> Result<()> {
    let tokens = CodexTokens::from_response(response)?;
    if tokens.refresh_token.is_none() {
        bail!("codex: login response lacked a refresh token");
    }
    auth::save_codex(home, &tokens)
}

/// Exchange an authorization or refresh grant at `/oauth/token`.
pub(crate) async fn token_request(params: &[(&str, &str)]) -> Result<Value> {
    let mut form = String::new();
    for (key, value) in params
        .iter()
        .copied()
        .chain(std::iter::once(("client_id", CLIENT_ID)))
    {
        if !form.is_empty() {
            form.push('&');
        }
        form.push_str(&percent_encode(key));
        form.push('=');
        form.push_str(&percent_encode(value));
    }
    let response = client()?
        .post(format!("{ISSUER}/oauth/token"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
        .map_err(|error| {
            anyhow::anyhow!("codex: token request failed; check connectivity ({error})")
        })?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    if status >= 400 {
        bail!("codex: token request failed (HTTP {status}); log in again");
    }
    serde_json::from_str(&text).context("codex: token response was not JSON")
}

async fn post_json(url: &str, body: Value) -> Result<(u16, Value)> {
    let response = client()?
        .post(url)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body)?)
        .send()
        .await
        .map_err(|error| anyhow::anyhow!("codex: request failed; check connectivity ({error})"))?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((status, value))
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("codex: could not build an HTTP client")
}

fn open_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => match (hex(bytes.get(index + 1)), hex(bytes.get(index + 2))) {
                (Some(high), Some(low)) => {
                    out.push(high << 4 | low);
                    index += 3;
                }
                _ => {
                    out.push(b'%');
                    index += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: Option<&u8>) -> Option<u8> {
    let byte = *byte?;
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(unix)]
fn random_bytes(count: usize) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut file = std::fs::File::open("/dev/urandom").context("codex: open /dev/urandom")?;
    let mut bytes = vec![0; count];
    file.read_exact(&mut bytes)
        .context("codex: read /dev/urandom")?;
    Ok(bytes)
}

#[cfg(not(unix))]
fn random_bytes(_count: usize) -> Result<Vec<u8>> {
    bail!("codex: no secure random source on this platform")
}

pub(crate) fn b64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk[0] as u32;
        let second = chunk.get(1).copied().unwrap_or(0) as u32;
        let third = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = first << 16 | second << 8 | third;
        out.push(ALPHABET[(value >> 18) as usize & 63] as char);
        out.push(ALPHABET[(value >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(value >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[value as usize & 63] as char);
        }
    }
    out
}

pub(crate) fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut value = 0_u32;
    let mut bits = 0;
    for byte in text.trim_end_matches('=').bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32;
        value = value << 6 | digit;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((value >> bits) as u8);
            value &= (1 << bits) - 1;
        }
    }
    Some(out)
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.as_chunks::<64>().0 {
        let mut schedule = [0_u32; 64];
        for (index, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            schedule[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(SHA256_K[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut out = [0_u8; 32];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(&[0_u8; 64])),
            "f5a5fd42d16a20302798ef6ed309979b43003d2320d9f0e8ea9831a92759fb4b"
        );
    }

    #[test]
    fn base64url_has_no_padding_and_round_trips() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(b"foob"), "Zm9vYg");
        assert_eq!(b64url(b"fooba"), "Zm9vYmE");
        assert_eq!(b64url(b"foobar"), "Zm9vYmFy");
        for text in ["", "f", "fo", "foo", "foobar", "\u{ff}\u{fe}\u{fd}"] {
            let bytes = b64url_decode(&b64url(text.as_bytes())).unwrap();
            assert_eq!(bytes, text.as_bytes());
        }
        assert_eq!(b64url_decode("Zm9vYg==").unwrap(), b"foob");
        assert_eq!(b64url_decode("not base64!"), None);
    }

    #[test]
    fn pkce_challenge_matches_the_rfc_example() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            b64url(&sha256(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_carries_the_expected_parameters() {
        let url = authorize_url("challenge", "state");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        for part in [
            "response_type=code",
            "client_id=app_EMoamEEZ73f0CkXaXp7hrann",
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
            "scope=openid%20profile%20email%20offline_access",
            "code_challenge=challenge",
            "code_challenge_method=S256",
            "state=state",
            "id_token_add_organizations=true",
            "codex_cli_simplified_flow=true",
            "originator=yoke",
        ] {
            assert!(url.contains(part), "{part} missing from {url}");
        }
    }

    #[test]
    fn callback_accepts_only_the_right_state_and_code() {
        let good = "GET /auth/callback?code=abc&state=xyz HTTP/1.1";
        assert_eq!(callback_request(good, "xyz").unwrap().unwrap(), "abc");
        assert!(callback_request(good, "nope").unwrap().is_err());
        assert!(
            callback_request("GET /auth/callback?state=xyz HTTP/1.1", "xyz")
                .unwrap()
                .is_err()
        );
        assert!(callback_request(
            "GET /auth/callback?error=access_denied&state=xyz HTTP/1.1",
            "xyz"
        )
        .unwrap()
        .is_err());
        assert!(callback_request("GET /favicon.ico HTTP/1.1", "xyz").is_none());
        assert_eq!(
            callback_request("GET /auth/callback?code=a%2Fb&state=x%20y HTTP/1.1", "x y")
                .unwrap()
                .unwrap(),
            "a/b"
        );
        assert!(constant_time_eq("same", "same"));
        assert!(!constant_time_eq("same", "diff"));
    }

    fn request() -> crate::types::Request {
        use crate::types::{Message, Part, Role};

        crate::types::Request {
            model: DEFAULT_MODEL.into(),
            instructions: "Be brief.".into(),
            messages: vec![
                Message {
                    role: Role::User,
                    parts: vec![Part::Text {
                        text: "What is in a.txt?".into(),
                    }],
                },
                Message {
                    role: Role::Assistant,
                    parts: vec![Part::Reasoning {
                        text: "Need to read".into(),
                        raw: Some(json!({
                            "id": "rs_1",
                            "type": "reasoning",
                            "summary": [{"type": "summary_text", "text": "Need to read"}],
                            "encrypted_content": "gAAA",
                        })),
                    }],
                },
            ],
            tools: vec![json!({"name": "read", "parameters": {"type": "object"}})],
            effort: Some("low".into()),
            conversation_id: Some("conv-123".into()),
        }
    }

    #[test]
    fn body_carries_the_codex_quirks() {
        use crate::providers::Provider;

        let home = crate::tools::testutil::TempDir::new("codex");
        let value = new(home.path()).body(&request());
        assert_eq!(value["model"], DEFAULT_MODEL);
        assert_eq!(value["instructions"], "Be brief.");
        assert_eq!(value["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(value["prompt_cache_key"], "conv-123");
        assert_eq!(
            value["reasoning"],
            json!({"summary": "auto", "effort": "low"})
        );
        assert_eq!(value["input"][1]["encrypted_content"], "gAAA");
        assert!(value.get("session_id").is_none());
        assert!(value.get("max_output_tokens").is_none());
    }

    #[tokio::test]
    async fn headers_carry_codex_auth_and_the_session_id() {
        use crate::auth::Auth;

        let home = crate::tools::testutil::TempDir::new("codex");
        auth::save_codex(
            home.path(),
            &CodexTokens {
                refresh_token: Some("refresh".into()),
                access_token: Some("access".into()),
                expires_at: Some(2_000),
                ..CodexTokens::default()
            },
        )
        .unwrap();
        let auth = auth::CodexAuth::with_clock(home.path(), || 1_000);
        let headers =
            responses::request_headers(&quirks(), Some("conv-123"), auth.headers().await.unwrap());
        assert_eq!(
            headers,
            [
                ("content-type", "application/json"),
                ("accept", "text/event-stream"),
                ("authorization", "Bearer access"),
                ("originator", "yoke"),
                ("user-agent", concat!("yoke/", env!("CARGO_PKG_VERSION"))),
                ("session_id", "conv-123"),
            ]
            .map(|(key, value)| (key.to_string(), value.to_string()))
        );
    }
}

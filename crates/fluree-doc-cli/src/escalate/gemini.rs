//! Google Vertex AI as the deep reader.
//!
//! Service-account credentials only. A key file is a thing a user can put on
//! a machine and revoke centrally, which is what a batch job wants; the
//! interactive flows are for a person at a terminal and would make every
//! unattended run a login prompt.
//!
//! Auth is the documented JWT-bearer exchange: sign a short assertion with
//! the key's private half, trade it for an access token, use the token until
//! it expires. Nothing here is Google-specific beyond the endpoints, so a
//! second provider is a second module rather than a rework of this one.

use base64::Engine;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long an assertion — and so the token it buys — is good for. An hour is
/// the maximum Google accepts, and a batch is minutes.
const TOKEN_TTL_SECS: u64 = 3600;

/// Refresh this long before expiry, so a request never carries a token that
/// dies in flight.
const REFRESH_MARGIN_SECS: u64 = 60;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// The fields this reads from a service-account key file.
struct ServiceAccount {
    client_email: String,
    private_key: String,
    project_id: String,
}

pub struct Reader {
    account: ServiceAccount,
    project: String,
    model: String,
    agent: ureq::Agent,
    /// `(token, unix expiry)`, minted on first use.
    token: std::sync::Mutex<Option<(String, u64)>>,
}

impl Reader {
    /// Load a reader from a service-account key.
    ///
    /// Fails here rather than at the first request: a batch that dies on
    /// document forty because the key was malformed has already spent forty
    /// documents' worth of someone's time.
    pub fn open(
        credentials: &std::path::Path,
        project: Option<&str>,
        model: &str,
    ) -> Result<Reader, String> {
        let text = std::fs::read_to_string(credentials)
            .map_err(|e| format!("{}: {e}", credentials.display()))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", credentials.display()))?;
        let field = |k: &str| -> Result<String, String> {
            v.get(k)
                .and_then(serde_json::Value::as_str)
                .map(String::from)
                .ok_or_else(|| format!("{}: no {k}", credentials.display()))
        };
        let account = ServiceAccount {
            client_email: field("client_email")?,
            private_key: field("private_key")?,
            project_id: field("project_id")?,
        };
        let project = project.unwrap_or(&account.project_id).to_string();
        Ok(Reader {
            account,
            project,
            model: model.to_string(),
            agent: ureq::Agent::new_with_defaults(),
            token: std::sync::Mutex::new(None),
        })
    }

    /// A valid access token, minting one if the held token is gone or nearly.
    fn token(&self) -> Result<String, String> {
        let now = unix_now();
        let mut held = self.token.lock().map_err(|_| "token lock poisoned")?;
        if let Some((t, expiry)) = held.as_ref() {
            if *expiry > now + REFRESH_MARGIN_SECS {
                return Ok(t.clone());
            }
        }
        let assertion = self.assertion(now)?;
        let body =
            format!("grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion={assertion}");
        let mut resp = self
            .agent
            .post(TOKEN_URL)
            .content_type("application/x-www-form-urlencoded")
            .send(&body)
            .map_err(|e| format!("token request failed: {e}"))?;
        let v: serde_json::Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("token response: {e}"))?;
        let token = v
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("token response had no access_token: {v}"))?
            .to_string();
        *held = Some((token.clone(), now + TOKEN_TTL_SECS));
        Ok(token)
    }

    /// The signed JWT that buys a token.
    fn assertion(&self, now: u64) -> Result<String, String> {
        use rsa::pkcs1v15::SigningKey;
        use rsa::pkcs8::DecodePrivateKey;
        use rsa::signature::{SignatureEncoding, Signer};
        use rsa::RsaPrivateKey;

        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = b64.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = serde_json::json!({
            "iss": self.account.client_email,
            "scope": SCOPE,
            "aud": TOKEN_URL,
            "iat": now,
            "exp": now + TOKEN_TTL_SECS,
        });
        let claims = b64.encode(serde_json::to_vec(&claims).map_err(|e| e.to_string())?);
        let signing_input = format!("{header}.{claims}");

        let key = RsaPrivateKey::from_pkcs8_pem(&self.account.private_key)
            .map_err(|e| format!("private_key is not a PKCS#8 RSA key: {e}"))?;
        let signature = SigningKey::<sha2::Sha256>::new(key).sign(signing_input.as_bytes());
        Ok(format!(
            "{signing_input}.{}",
            b64.encode(signature.to_bytes())
        ))
    }

    /// Read one crop rendered by us, which is always a PNG.
    pub fn read(&self, png: &[u8], prompt: &str) -> Result<Option<String>, String> {
        self.read_typed(png, "image/png", prompt)
    }

    /// Read one image of a stated media type.
    ///
    /// Separate from [`Reader::read`] because a source image is passed through
    /// as it arrived: re-encoding a JPEG to PNG to fit one code path would
    /// spend a decode and could only lose detail the reader might have used.
    /// Returns the model's text, or `None` where it returned nothing — a real
    /// answer for an image with nothing printed on it.
    pub fn read_typed(
        &self,
        bytes: &[u8],
        mime: &str,
        prompt: &str,
    ) -> Result<Option<String>, String> {
        let url = format!(
            "https://aiplatform.googleapis.com/v1/projects/{}/locations/global\
             /publishers/google/models/{}:generateContent",
            self.project, self.model
        );
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "inlineData": { "mimeType": mime,
                                      "data": base64::engine::general_purpose::STANDARD.encode(bytes) } },
                    { "text": prompt },
                ],
            }],
            "generationConfig": {
                // Transcription, not composition: the same pixels should give
                // the same reading every time.
                "temperature": 0,
                "maxOutputTokens": 8000,
                // Thinking is capped deliberately. Measured across reasoning
                // levels, more of it makes transcription *worse* -- the model
                // starts reconciling what it reads with what it expects a
                // document to say -- as well as several times slower.
                "thinkingConfig": if self.model.starts_with("gemini-3") {
                    serde_json::json!({ "thinkingLevel": "LOW" })
                } else {
                    serde_json::json!({ "thinkingBudget": 0 })
                },
            },
        });
        let mut last = String::new();
        for attempt in 0..RETRIES {
            let token = self.token()?;
            let sent = self
                .agent
                .post(&url)
                .header("Authorization", &format!("Bearer {token}"))
                .content_type("application/json")
                .send_json(&body);
            match sent {
                Ok(mut resp) => {
                    let v: serde_json::Value = resp
                        .body_mut()
                        .read_json()
                        .map_err(|e| format!("read response: {e}"))?;
                    // A reading that stopped early is not a short reading, it
                    // is half of one — and it looks exactly like a complete
                    // reading of a shorter page. Nothing downstream can tell
                    // the difference, and a page reading replaces a whole
                    // page, so this has to fail rather than truncate.
                    if let Some(reason) = incomplete(&v) {
                        return Err(format!("reading stopped early: {reason}"));
                    }
                    return Ok(text_of(&v));
                }
                Err(e) => {
                    last = e.to_string();
                    // Rate limits and the transient 5xx family are what a
                    // batch actually meets; anything else is a request this
                    // retry will not fix.
                    let retryable = matches!(status_of(&e), Some(429 | 500 | 502 | 503 | 504));
                    if !retryable || attempt + 1 == RETRIES {
                        return Err(format!("read failed: {last}"));
                    }
                    std::thread::sleep(std::time::Duration::from_secs(5 * (attempt as u64 + 1)));
                }
            }
        }
        Err(format!("read failed: {last}"))
    }
}

/// Attempts per crop, including the first.
const RETRIES: u32 = 4;

fn status_of(e: &ureq::Error) -> Option<u16> {
    match e {
        ureq::Error::StatusCode(c) => Some(*c),
        _ => None,
    }
}

/// Why a response is not a complete reading, if it is not one.
///
/// `STOP` is the finish this wants. `MAX_TOKENS` means the output budget ran
/// out mid-page; the safety and recitation finishes mean the candidate was
/// withheld. An absent `finishReason` on a candidate that carries text is
/// accepted — some responses omit it — but an absent *candidate* is not a
/// reading at all.
fn incomplete(v: &serde_json::Value) -> Option<String> {
    let candidates = v.get("candidates").and_then(serde_json::Value::as_array);
    let Some(first) = candidates.and_then(|c| c.first()) else {
        // A request that produced no candidate usually says why.
        let why = v
            .get("promptFeedback")
            .and_then(|f| f.get("blockReason"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("no candidate in the response");
        return Some(why.to_string());
    };
    match first
        .get("finishReason")
        .and_then(serde_json::Value::as_str)
    {
        None | Some("STOP") => None,
        Some(other) => Some(other.to_string()),
    }
}

/// The transcription out of a generateContent response.
///
/// A part carrying `thought` is the model reasoning about the image, not a
/// reading of it, and concatenating the two puts commentary into a document.
fn text_of(v: &serde_json::Value) -> Option<String> {
    let parts = v
        .get("candidates")?
        .as_array()?
        .first()?
        .get("content")?
        .get("parts")?
        .as_array()?;
    let joined: String = parts
        .iter()
        .filter(|p| p.get("thought").and_then(serde_json::Value::as_bool) != Some(true))
        .filter_map(|p| p.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_is_taken_from_the_first_candidate() {
        let v = serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "# Title\n\nbody" }] } }]
        });
        assert_eq!(text_of(&v).as_deref(), Some("# Title\n\nbody"));
    }

    #[test]
    fn a_thought_part_is_not_part_of_the_reading() {
        let v = serde_json::json!({
            "candidates": [{ "content": { "parts": [
                { "text": "Let me look at the layout…", "thought": true },
                { "text": "the actual reading" },
            ] } }]
        });
        assert_eq!(text_of(&v).as_deref(), Some("the actual reading"));
    }

    #[test]
    fn a_truncated_reading_is_refused_rather_than_taken_short() {
        let v = serde_json::json!({
            "candidates": [{
                "finishReason": "MAX_TOKENS",
                "content": { "parts": [{ "text": "# Half a page" }] }
            }]
        });
        assert_eq!(incomplete(&v).as_deref(), Some("MAX_TOKENS"));
    }

    #[test]
    fn a_normal_finish_and_a_missing_one_both_pass() {
        let stop = serde_json::json!({
            "candidates": [{ "finishReason": "STOP", "content": { "parts": [{ "text": "x" }] } }]
        });
        assert_eq!(incomplete(&stop), None);
        let absent = serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "x" }] } }]
        });
        assert_eq!(incomplete(&absent), None);
    }

    #[test]
    fn a_blocked_request_reports_why() {
        let v = serde_json::json!({ "promptFeedback": { "blockReason": "SAFETY" } });
        assert_eq!(incomplete(&v).as_deref(), Some("SAFETY"));
        assert!(incomplete(&serde_json::json!({})).is_some());
    }

    #[test]
    fn an_empty_reading_is_none_rather_than_an_empty_element() {
        let v = serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "   " }] } }]
        });
        assert_eq!(text_of(&v), None);
        assert_eq!(text_of(&serde_json::json!({})), None);
    }
}

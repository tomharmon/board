use super::*;
use ring::hmac;

/// Authenticate the original bytes before parsing or redaction. Never persist the secret or signature.
pub fn verify_intake(
    provider: &Provider,
    secret: &[u8],
    body: &[u8],
    signature: &str,
    timestamp: Option<&str>,
    now: i64,
) -> Result<()> {
    ensure!(
        !secret.is_empty() && body.len() <= 2 * 1024 * 1024,
        "invalid signing secret or event size"
    );
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let sig = match provider {
        Provider::Github => signature.strip_prefix("sha256="),
        Provider::Slack => signature.strip_prefix("v0="),
        Provider::Linear => Some(signature),
        Provider::Local => bail!("local envelopes are not provider webhooks"),
    }
    .context("unexpected provider signature format")?;
    ensure!(sig.len() == 64, "invalid signature length");
    let tag = hex::decode(sig).context("invalid signature encoding")?;
    let input = if *provider == Provider::Slack {
        let t = timestamp.context("Slack verification needs --timestamp")?;
        ensure!(
            t.bytes().all(|n| n.is_ascii_digit()) && now.abs_diff(t.parse::<i64>()?) <= 300,
            "Slack event outside five-minute window"
        );
        let mut b = format!("v0:{t}:").into_bytes();
        b.extend_from_slice(body);
        std::borrow::Cow::Owned(b)
    } else {
        std::borrow::Cow::Borrowed(body)
    };
    hmac::verify(&key, &input, &tag)
        .map_err(|_| anyhow::anyhow!("provider signature did not verify"))?;
    if *provider == Provider::Linear {
        let data: Value = serde_json::from_slice(body)?;
        let ts = data["webhookTimestamp"]
            .as_i64()
            .context("missing Linear webhookTimestamp")?;
        ensure!(
            now.saturating_mul(1000).abs_diff(ts) <= 60_000,
            "Linear event outside one-minute window"
        );
    }
    Ok(())
}

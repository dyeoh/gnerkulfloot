//! The `hitpay` adapter: HitPay's hosted checkout (DuitNow QR, Touch 'n Go,
//! cards and more).
//!
//! HitPay's webhook payloads vary in shape between event types and docs
//! versions, so we use the webhook only as a signed "something changed" signal:
//! after checking its signature we fetch the payment request from HitPay's API
//! and act on that answer alone.

use async_trait::async_trait;
use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use iso_currency::Currency;
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;

use super::{CreatedPayment, PaymentAdapter, PaymentError, PaymentRequest, RemoteState, RemoteStatus};
use crate::{config::HitpayConfig, money::Money};

const PRODUCTION_API: &str = "https://api.hit-pay.com";
const SANDBOX_API: &str = "https://api.sandbox.hit-pay.com";
const SIGNATURE_HEADER: &str = "hitpay-signature";
const EVENT_OBJECT_HEADER: &str = "hitpay-event-object";

pub struct Hitpay {
    cfg: HitpayConfig,
    api_base: String,
    http: reqwest::Client,
}

impl Hitpay {
    pub fn new(cfg: HitpayConfig) -> Self {
        let api_base = cfg
            .api_base
            .clone()
            .unwrap_or_else(|| if cfg.sandbox { SANDBOX_API } else { PRODUCTION_API }.to_owned());
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("building an HTTP client with static settings cannot fail");
        Self {
            cfg,
            api_base: api_base.trim_end_matches('/').to_owned(),
            http,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.api_base))
            .header("X-BUSINESS-API-KEY", &self.cfg.api_key)
            .header("X-Requested-With", "XMLHttpRequest")
    }
}

#[derive(Deserialize)]
struct CreatedResponse {
    id: String,
    url: Option<String>,
}

/// HitPay sends amounts as `"29.20"` in some responses and `29.2` in others.
fn parse_amount(value: &Value, currency: Currency) -> Result<Money, PaymentError> {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => return Err(PaymentError::Provider(format!("unexpected amount {other}"))),
    };
    Money::parse_decimal(&text, currency).map_err(|e| PaymentError::Provider(e.to_string()))
}

async fn error_for(res: reqwest::Response) -> PaymentError {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    PaymentError::Provider(format!(
        "HitPay answered {status}: {}",
        body.chars().take(500).collect::<String>()
    ))
}

#[async_trait]
impl PaymentAdapter for Hitpay {
    fn id(&self) -> &'static str {
        "hitpay"
    }

    async fn create(&self, req: &PaymentRequest) -> Result<CreatedPayment, PaymentError> {
        let mut form: Vec<(&str, String)> = vec![
            ("amount", req.amount.to_decimal_string()),
            ("currency", req.amount.currency.code().to_owned()),
            ("email", req.email.clone()),
            ("name", req.name.clone()),
            ("purpose", format!("Order #{}", req.order_number)),
            ("reference_number", req.order_id.to_string()),
            // The request expires with the order, so nobody pays for an order
            // whose stock has already gone back on sale.
            ("expires_after", format!("{} mins", req.expires_in_minutes.max(1))),
        ];
        if let Some(url) = &self.cfg.return_url {
            form.push(("redirect_url", url.replace("{order_id}", &req.order_id.to_string())));
        }
        for method in &self.cfg.payment_methods {
            form.push(("payment_methods[]", method.clone()));
        }
        let res = self
            .request(reqwest::Method::POST, "/v1/payment-requests")
            .form(&form)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(error_for(res).await);
        }
        let created: CreatedResponse = res.json().await?;
        Ok(CreatedPayment {
            provider_ref: created.id,
            checkout_url: created.url,
            qr_code: None,
        })
    }

    fn verify_webhook(&self, headers: &HeaderMap, body: &[u8]) -> Result<Option<String>, PaymentError> {
        let signature = headers
            .get(SIGNATURE_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or(PaymentError::InvalidSignature)?;
        let expected = decode_hex(signature.trim()).ok_or(PaymentError::InvalidSignature)?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.cfg.webhook_salt.as_bytes()).expect("HMAC accepts keys of any length");
        mac.update(body);
        // verify_slice compares in constant time.
        mac.verify_slice(&expected)
            .map_err(|_| PaymentError::InvalidSignature)?;

        // Genuine, but maybe about something else (orders, payouts…).
        let object = headers.get(EVENT_OBJECT_HEADER).and_then(|v| v.to_str().ok());
        if object.is_some_and(|o| o != "payment_request") {
            return Ok(None);
        }
        let payload: Value = serde_json::from_slice(body).map_err(|e| PaymentError::Provider(e.to_string()))?;
        Ok(payload.get("id").and_then(Value::as_str).map(str::to_owned))
    }

    async fn fetch_status(&self, provider_ref: &str, currency: Currency) -> Result<RemoteStatus, PaymentError> {
        let res = self
            .request(reqwest::Method::GET, &format!("/v1/payment-requests/{provider_ref}"))
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(error_for(res).await);
        }
        let body: Value = res.json().await?;
        let remote_currency = body.get("currency").and_then(Value::as_str).unwrap_or_default();
        if !remote_currency.eq_ignore_ascii_case(currency.code()) {
            return Err(PaymentError::Provider(format!(
                "payment request {provider_ref} is in {remote_currency}, expected {}",
                currency.code()
            )));
        }
        let amount = parse_amount(body.get("amount").unwrap_or(&Value::Null), currency)?;
        let state = match body.get("status").and_then(Value::as_str) {
            Some("completed") => RemoteState::Succeeded,
            Some("failed") => RemoteState::Failed,
            _ => RemoteState::Pending,
        };
        Ok(RemoteStatus { state, amount })
    }
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> Hitpay {
        Hitpay::new(HitpayConfig {
            api_key: "key".into(),
            webhook_salt: "salt".into(),
            sandbox: true,
            payment_methods: vec![],
            return_url: None,
            api_base: None,
        })
    }

    fn sign(body: &[u8], salt: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(salt.as_bytes()).unwrap();
        mac.update(body);
        mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn headers(signature: &str, object: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(SIGNATURE_HEADER, signature.parse().unwrap());
        if let Some(o) = object {
            h.insert(EVENT_OBJECT_HEADER, o.parse().unwrap());
        }
        h
    }

    #[test]
    fn accepts_only_correctly_signed_webhooks() {
        let body = br#"{"id":"pr_123","status":"completed"}"#;
        let a = adapter();
        let good = sign(body, "salt");
        assert_eq!(
            a.verify_webhook(&headers(&good, Some("payment_request")), body)
                .unwrap()
                .as_deref(),
            Some("pr_123")
        );
        assert!(
            a.verify_webhook(&headers(&sign(body, "wrong salt"), None), body)
                .is_err()
        );
        assert!(a.verify_webhook(&headers("zz", None), body).is_err());
        assert!(a.verify_webhook(&HeaderMap::new(), body).is_err());
        // Any change to the body breaks the signature.
        assert!(
            a.verify_webhook(&headers(&good, None), br#"{"id":"pr_999","status":"completed"}"#)
                .is_err()
        );
        // Genuine but about another kind of object: ignored, not an error.
        assert_eq!(a.verify_webhook(&headers(&good, Some("payout")), body).unwrap(), None);
    }

    #[test]
    fn parses_amounts_in_both_shapes() {
        assert_eq!(parse_amount(&Value::from("29.20"), Currency::MYR).unwrap().amount, 2920);
        assert_eq!(
            parse_amount(&serde_json::json!(29.2), Currency::MYR).unwrap().amount,
            2920
        );
        assert_eq!(
            parse_amount(&serde_json::json!(100), Currency::MYR).unwrap().amount,
            10000
        );
        assert!(parse_amount(&Value::Null, Currency::MYR).is_err());
    }
}

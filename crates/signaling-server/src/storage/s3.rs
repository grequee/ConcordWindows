//! Minimal path-style S3 GET/PUT/DELETE with AWS Signature V4.
//! Only server-generated keys inside concord-images/ are accepted.

use super::{Objects, StorageError, http_client, read_limited};
use async_trait::async_trait;
use chrono::Utc;
use concord_protocol::MAX_CHAT_IMAGE_BYTES;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use url::Url;

pub(super) struct Settings {
    endpoint: Url,
    region: String,
    bucket: String,
    access: String,
    secret: String,
}

impl Settings {
    pub(super) fn read(get: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let endpoint = get("AWS_ENDPOINT_URL_S3");
        let bucket = get("CONCORD_STORAGE_BUCKET");
        let access = get("AWS_ACCESS_KEY_ID");
        let secret = get("AWS_SECRET_ACCESS_KEY");
        let region = get("AWS_REGION");
        if [&endpoint, &bucket, &access, &secret, &region]
            .iter()
            .all(|value| value.is_none())
        {
            return Ok(None);
        }
        let required = |value: Option<String>, key| {
            value
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow::anyhow!("missing {key}"))
        };
        let endpoint = Url::parse(&required(endpoint, "AWS_ENDPOINT_URL_S3")?)
            .map_err(|_| anyhow::anyhow!("invalid AWS_ENDPOINT_URL_S3"))?;
        anyhow::ensure!(
            endpoint.scheme() == "https"
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.path() == "/",
            "S3 endpoint must be an HTTPS origin without credentials or a path"
        );
        let bucket = required(bucket, "CONCORD_STORAGE_BUCKET")?;
        anyhow::ensure!(
            (3..=63).contains(&bucket.len())
                && bucket.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'-'
                    || b == b'.'),
            "invalid storage bucket name"
        );
        let region = required(region, "AWS_REGION")?;
        let access = required(access, "AWS_ACCESS_KEY_ID")?;
        anyhow::ensure!(
            [&region, &access].iter().all(|s| s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))),
            "invalid S3 region/access key characters"
        );
        Ok(Some(Self {
            endpoint,
            region,
            bucket,
            access,
            secret: required(secret, "AWS_SECRET_ACCESS_KEY")?,
        }))
    }
}

pub(super) struct Bucket {
    settings: Settings,
    client: reqwest::Client,
}

impl Bucket {
    pub(super) fn new(settings: Settings) -> anyhow::Result<Self> {
        Ok(Self {
            settings,
            client: http_client()?,
        })
    }

    async fn request(
        &self,
        method: reqwest::Method,
        key: &str,
        data: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<reqwest::Response, StorageError> {
        if !key.starts_with("concord-images/")
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-/".contains(&b))
            || key.contains("..")
        {
            return Err(StorageError::Invalid("Chave de imagem inválida."));
        }
        let mut url = self.settings.endpoint.clone();
        url.set_path(&format!("/{}/{key}", self.settings.bucket));
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let payload = format!("{:x}", Sha256::digest(&data));
        let authorization = authorization(
            &self.settings,
            method.as_str(),
            url.path(),
            &host,
            &stamp,
            &payload,
        )?;
        let mut auth = reqwest::header::HeaderValue::from_str(&authorization)
            .map_err(|_| StorageError::Unavailable)?;
        auth.set_sensitive(true);
        let mut request = self
            .client
            .request(method, url)
            .header("authorization", auth)
            .header("x-amz-date", stamp)
            .header("x-amz-content-sha256", payload)
            .body(data);
        if let Some(content_type) = content_type {
            request = request
                .header("content-type", content_type)
                .header("content-disposition", "inline");
        }
        let response = request
            .send()
            .await
            .map_err(|_| StorageError::Unavailable)?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().as_u16();
        let body = read_limited(response, 16 * 1024).await?;
        Err(classify_error(status, &String::from_utf8_lossy(&body)))
    }
}

fn classify_error(status: u16, body: &str) -> StorageError {
    if status == 404 {
        return StorageError::NotFound;
    }
    if status == 507
        || [
            "QuotaExceeded",
            "StorageLimitExceeded",
            "BucketQuotaExceeded",
            "InsufficientStorage",
        ]
        .iter()
        .any(|code| body.contains(&format!("<Code>{code}</Code>")))
    {
        return StorageError::Capacity;
    }
    StorageError::Unavailable
}

fn hmac(key: &[u8], value: &str) -> Result<Vec<u8>, StorageError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| StorageError::Unavailable)?;
    mac.update(value.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

fn authorization(
    settings: &Settings,
    method: &str,
    path: &str,
    host: &str,
    stamp: &str,
    payload: &str,
) -> Result<String, StorageError> {
    let date = stamp.get(..8).ok_or(StorageError::Unavailable)?;
    let signed = "host;x-amz-content-sha256;x-amz-date";
    let canonical = format!(
        "{method}\n{path}\n\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{stamp}\n\n{signed}\n{payload}"
    );
    let scope = format!("{date}/{}/s3/aws4_request", settings.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{:x}",
        Sha256::digest(canonical.as_bytes())
    );
    let day = hmac(format!("AWS4{}", settings.secret).as_bytes(), date)?;
    let region = hmac(&day, &settings.region)?;
    let service = hmac(&region, "s3")?;
    let signing = hmac(&service, "aws4_request")?;
    let signature = hmac(&signing, &to_sign)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
        settings.access
    ))
}

#[async_trait]
impl Objects for Bucket {
    async fn put(&self, key: &str, data: Vec<u8>, content_type: &str) -> Result<(), StorageError> {
        self.request(reqwest::Method::PUT, key, data, Some(content_type))
            .await?;
        Ok(())
    }
    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        let response = self
            .request(reqwest::Method::GET, key, vec![], None)
            .await?;
        read_limited(response, MAX_CHAT_IMAGE_BYTES).await
    }
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        match self
            .request(reqwest::Method::DELETE, key, vec![], None)
            .await
        {
            Ok(_) | Err(StorageError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn access_errors_and_throttling_do_not_trigger_deletion() {
        for (status, body) in [
            (403, "<Code>AccessDenied</Code>"),
            (403, "<Code>SignatureDoesNotMatch</Code>"),
            (503, "<Code>SlowDown</Code>"),
            (429, ""),
        ] {
            assert!(matches!(
                classify_error(status, body),
                StorageError::Unavailable
            ));
        }
        assert!(matches!(classify_error(507, ""), StorageError::Capacity));
        assert!(matches!(
            classify_error(403, "<Code>QuotaExceeded</Code>"),
            StorageError::Capacity
        ));
    }
}

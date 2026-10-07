//! Image and file storage. Uses the `object_store` crate directly: it is
//! already a vendor-neutral interface over S3-compatible buckets (AWS, R2,
//! DigitalOcean Spaces, MinIO) and local disk, so we don't wrap it in our own
//! adapter trait. This module only adds config wiring and public URLs.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use bytes::Bytes;
use object_store::{
    Attribute, AttributeValue, Attributes, ObjectStore, PutOptions, PutPayload, aws::AmazonS3Builder,
    local::LocalFileSystem, path::Path as ObjectPath,
};

use crate::config::StorageConfig;

/// Content-addressed files never change, so browsers and CDNs may cache them forever.
pub const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";

#[derive(Clone)]
pub struct Storage {
    store: Arc<dyn ObjectStore>,
    public_base_url: String,
    /// Set for the local adapter, so the HTTP layer can serve the files.
    local_root: Option<PathBuf>,
}

impl Storage {
    pub fn from_config(cfg: &StorageConfig) -> anyhow::Result<Self> {
        match cfg {
            StorageConfig::Local { path, public_base_url } => {
                std::fs::create_dir_all(path)
                    .with_context(|| format!("creating media directory {}", path.display()))?;
                let root = path.canonicalize()?;
                Ok(Self {
                    store: Arc::new(LocalFileSystem::new_with_prefix(&root)?),
                    public_base_url: public_base_url.trim_end_matches('/').to_owned(),
                    local_root: Some(root),
                })
            }
            StorageConfig::S3 {
                bucket,
                region,
                endpoint,
                access_key_id,
                secret_access_key,
                public_base_url,
                allow_http,
            } => {
                let mut builder = AmazonS3Builder::new()
                    .with_bucket_name(bucket)
                    .with_region(region)
                    .with_access_key_id(access_key_id)
                    .with_secret_access_key(secret_access_key)
                    .with_allow_http(*allow_http);
                if let Some(endpoint) = endpoint {
                    // Custom endpoints (R2, Spaces, MinIO) address buckets by path.
                    builder = builder.with_endpoint(endpoint).with_virtual_hosted_style_request(false);
                }
                Ok(Self {
                    store: Arc::new(builder.build().context("configuring S3 storage")?),
                    public_base_url: public_base_url.trim_end_matches('/').to_owned(),
                    local_root: None,
                })
            }
        }
    }

    /// Writes a file. On S3-compatible stores the object carries its content
    /// type and a long-lived cache header, which the bucket passes on to
    /// browsers. Local disk can't store headers; the `/media` route adds them
    /// when serving instead.
    pub async fn put(&self, key: &str, bytes: Bytes, content_type: &'static str) -> object_store::Result<()> {
        let mut opts = PutOptions::default();
        if self.local_root.is_none() {
            opts.attributes = Attributes::from_iter([
                (Attribute::ContentType, AttributeValue::from(content_type)),
                (Attribute::CacheControl, AttributeValue::from(IMMUTABLE_CACHE)),
            ]);
        }
        self.store
            .put_opts(&ObjectPath::from(key), PutPayload::from(bytes), opts)
            .await?;
        Ok(())
    }

    /// Deletes a file. Missing files are not an error.
    pub async fn delete(&self, key: &str) -> object_store::Result<()> {
        match self.store.delete(&ObjectPath::from(key)).await {
            Err(object_store::Error::NotFound { .. }) => Ok(()),
            other => other,
        }
    }

    /// The URL a browser should use to fetch `key`.
    pub fn url(&self, key: &str) -> String {
        format!("{}/{key}", self.public_base_url)
    }

    pub fn local_root(&self) -> Option<&PathBuf> {
        self.local_root.as_ref()
    }

    /// Fetches a file's bytes, mainly for tests and maintenance.
    pub async fn get(&self, key: &str) -> object_store::Result<Bytes> {
        self.store.get(&ObjectPath::from(key)).await?.bytes().await
    }
}

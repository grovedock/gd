//! Generated gRPC types for the Grovedock protocol (`proto/gd.proto`) plus the
//! transport settings and auth interceptor shared by every client and tier.
pub mod v1 {
    tonic::include_proto!("gd.v1");
}
/// Canonical object encodings (`proto/gd_objects.proto`), published for SDKs and
/// third-party tools. The core crate's own types must stay byte-compatible with these.
pub mod objects {
    tonic::include_proto!("gd.objects.v1");
}
pub use v1::*;

/// Encoded descriptor set for gRPC server reflection (grpcurl, Postman, etc.).
pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("gd_descriptor");

use tonic::metadata::{AsciiMetadataValue, MetadataValue};
use tonic::service::Interceptor;
use tonic::{Request, Status};

/// Largest single gRPC message: one max-size object plus framing.
pub const MAX_MESSAGE_BYTES: usize = 260 << 20;

/// Objects per streamed `Get`/`Missing` request chunk.
pub const OBJECT_BATCH: usize = 1024;

/// Attaches `authorization: Bearer <token>` when a token is configured.
#[derive(Clone, Default)]
pub struct BearerAuth {
    token: Option<AsciiMetadataValue>,
    repo: Option<AsciiMetadataValue>,
    work: Option<AsciiMetadataValue>,
}

/// The request header that names the repository a call is about, on servers hosting many.
pub const REPO_HEADER: &str = "x-gd-repo";

/// The request header that marks a call as the server's own work (an import, the git bridge),
/// with a key only that server and the tools it runs know. Taken from [`WORK_ENV`].
pub const WORK_HEADER: &str = "x-gd-work";
/// Where a tool run by the server finds that key.
pub const WORK_ENV: &str = "GD_SERVER_WORK";

impl BearerAuth {
    pub fn new(token: Option<&str>) -> Result<Self, Status> {
        let token = token
            .map(|t| MetadataValue::try_from(format!("Bearer {t}")).map_err(|_| Status::invalid_argument("token")))
            .transpose()?;
        let work = std::env::var(WORK_ENV).ok().and_then(|k| MetadataValue::try_from(k).ok());
        Ok(Self { token, repo: None, work })
    }

    /// Names the repository (`OWNER/NAME`) on every call.
    pub fn with_repo(mut self, repo: Option<&str>) -> Result<Self, Status> {
        self.repo = repo
            .map(|r| MetadataValue::try_from(r).map_err(|_| Status::invalid_argument(format!("repository `{r}`"))))
            .transpose()?;
        Ok(self)
    }
}

impl Interceptor for BearerAuth {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(v) = &self.token {
            req.metadata_mut().insert("authorization", v.clone());
        }
        if let Some(v) = &self.repo {
            req.metadata_mut().insert(REPO_HEADER, v.clone());
        }
        if let Some(v) = &self.work {
            req.metadata_mut().insert(WORK_HEADER, v.clone());
        }
        Ok(req)
    }
}

/// A gRPC endpoint for `url`. An `https://` server is verified against the system's trusted
/// roots; without this, tonic refuses https ("Connecting to HTTPS without TLS enabled").
pub fn endpoint(url: impl Into<String>) -> Result<tonic::transport::Endpoint, tonic::transport::Error> {
    let url = url.into();
    let https = url.starts_with("https://");
    let e = tonic::transport::Endpoint::from_shared(url)?;
    if https { e.tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots()) } else { Ok(e) }
}

/// Splits `https://host[:port]/OWNER/NAME` into the server address and the repository. A URL
/// with no path is the server's default repository; any other path is left alone.
pub fn split_repo_url(url: &str) -> (String, Option<String>) {
    let trimmed = url.trim_end_matches('/');
    let Some(scheme_end) = trimmed.find("://") else { return (trimmed.to_owned(), None) };
    let rest = &trimmed[scheme_end + 3..];
    let Some(slash) = rest.find('/') else { return (trimmed.to_owned(), None) };
    let path = &rest[slash + 1..];
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() == 2 && parts.iter().all(|p| !p.is_empty()) {
        (trimmed[..scheme_end + 3 + slash].to_owned(), Some(path.to_owned()))
    } else {
        (trimmed.to_owned(), None)
    }
}

#[cfg(test)]
mod endpoint_tests {
    /// An https server gets a TLS handshake. Before, tonic refused every https URL outright,
    /// so the CLI and runners couldn't reach a hosted server at all.
    #[tokio::test]
    async fn https_urls_speak_tls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                drop(s); // not a TLS server: the handshake fails, which is the point
            }
        });
        let err = super::endpoint(format!("https://localhost:{}", addr.port())).unwrap().connect().await.unwrap_err();
        let msg = format!("{err:?}");
        assert!(!msg.contains("without TLS"), "{msg}");
        assert!(super::endpoint(format!("http://{addr}")).is_ok());
    }
}

#[cfg(test)]
mod repo_url_tests {
    use super::split_repo_url;

    #[test]
    fn splits_owner_and_name_from_the_server_address() {
        assert_eq!(
            split_repo_url("https://gd.example.com/lumen/fleet"),
            ("https://gd.example.com".into(), Some("lumen/fleet".into()))
        );
        assert_eq!(split_repo_url("http://127.0.0.1:7420/a/b/"), ("http://127.0.0.1:7420".into(), Some("a/b".into())));
        assert_eq!(split_repo_url("http://127.0.0.1:7420"), ("http://127.0.0.1:7420".into(), None));
        assert_eq!(split_repo_url("http://127.0.0.1:7420/"), ("http://127.0.0.1:7420".into(), None));
        assert_eq!(split_repo_url("http://h/one"), ("http://h/one".into(), None));
    }
}

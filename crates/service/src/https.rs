//! An opt-in TLS endpoint for agent requests. No proxy or system CA fallback.
use crate::{Error, Result, files};
use hyper::Uri;
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
};
use serde::Deserialize;
use std::{
    net::{Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PEM_BYTES: usize = 128 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    version: u8,
    listen: SocketAddr,
    server_name: String,
    certificate: PathBuf,
    private_key: PathBuf,
    #[serde(default = "default_connections")]
    max_connections: usize,
}

fn default_connections() -> usize {
    16
}

pub(crate) struct Listener {
    pub socket: TcpListener,
    pub acceptor: TlsAcceptor,
    pub authority: Arc<str>,
    pub max_connections: usize,
}

fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    if !path.is_absolute() {
        return Err(Error::Https);
    }
    let bytes = files::read_private(path, MAX_PEM_BYTES)?;
    let certs: Vec<_> = CertificateDer::pem_slice_iter(&bytes)
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Https)?;
    if certs.is_empty() || certs.len() > 16 {
        return Err(Error::Https);
    }
    Ok(certs)
}

fn name(host: &str) -> Result<ServerName<'static>> {
    ServerName::try_from(host.to_owned()).map_err(|_| Error::Https)
}

fn authority(host: &str, port: u16) -> String {
    let host = if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    if port == 443 {
        host
    } else {
        format!("{host}:{port}")
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

impl Listener {
    pub async fn bind(path: &Path) -> Result<Self> {
        let settings: Settings =
            serde_json::from_slice(&files::read_private(path, 4096)?).map_err(|_| Error::Https)?;
        name(&settings.server_name)?;
        if settings.version != 1
            || settings.listen.ip().is_unspecified()
            || settings.listen.ip().is_multicast()
            || !settings.private_key.is_absolute()
            || !(1..=128).contains(&settings.max_connections)
        {
            return Err(Error::Https);
        }
        let certs = certificates(&settings.certificate)?;
        let bytes = files::read_private(&settings.private_key, MAX_PEM_BYTES)?;
        let mut keys = PrivateKeyDer::pem_slice_iter(&bytes);
        let key = keys.next().ok_or(Error::Https)?.map_err(|_| Error::Https)?;
        if keys.next().is_some() {
            return Err(Error::Https);
        }
        let mut config = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Https)?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|_| Error::Https)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config.max_early_data_size = 0;
        let socket = TcpListener::bind(settings.listen).await?;
        Ok(Self {
            authority: authority(&settings.server_name, socket.local_addr()?.port()).into(),
            socket,
            acceptor: TlsAcceptor::from(Arc::new(config)),
            max_connections: settings.max_connections,
        })
    }

    pub async fn accept(&self) -> TcpStream {
        loop {
            match self.socket.accept().await {
                Ok((stream, _)) => return stream,
                // A transient TCP accept error must not stop the local service.
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }
}

pub(crate) struct Client {
    host: String,
    port: u16,
    server_name: ServerName<'static>,
    pub authority: String,
    connector: TlsConnector,
}

fn origin(url: &str) -> Result<(String, u16)> {
    let uri: Uri = url.parse().map_err(|_| Error::Https)?;
    if uri.scheme_str() != Some("https")
        || url.contains('#')
        || uri
            .path_and_query()
            .is_some_and(|path| path.as_str() != "/")
    {
        return Err(Error::Https);
    }
    let endpoint = uri.authority().ok_or(Error::Https)?;
    let raw_host = endpoint.host();
    let suffix = endpoint
        .as_str()
        .strip_prefix(raw_host)
        .ok_or(Error::Https)?;
    let port = if suffix.is_empty() {
        443
    } else {
        let port: u16 = suffix
            .strip_prefix(':')
            .ok_or(Error::Https)?
            .parse()
            .map_err(|_| Error::Https)?;
        if endpoint.port_u16() != Some(port) || port == 0 {
            return Err(Error::Https);
        }
        port
    };
    let host = raw_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(raw_host);
    name(host)?;
    Ok((host.into(), port))
}

impl Client {
    pub fn new(url: &str, ca_cert: &Path) -> Result<Self> {
        let (host, port) = origin(url)?;
        let mut roots = RootCertStore::empty();
        for certificate in certificates(ca_cert)? {
            roots.add(certificate).map_err(|_| Error::Https)?;
        }
        let mut config = ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Https)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config.enable_early_data = false;
        Ok(Self {
            server_name: name(&host)?,
            authority: authority(&host, port),
            host,
            port,
            connector: TlsConnector::from(Arc::new(config)),
        })
    }

    pub async fn connect(&self) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let stream = TcpStream::connect((self.host.as_str(), self.port)).await?;
        self.connector
            .connect(self.server_name.clone(), stream)
            .await
            .map_err(|_| Error::Https)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_explicit_https_origins() {
        for invalid in [
            "http://example.test:8443",
            "https://user@example.test",
            "https://example.test?query=1",
            "https://example.test/#fragment",
            "https://example.test/v1/commands",
            "https://example.test:abc",
            "https://example.test:0",
            "https://example.test:65536",
            "example.test:443",
        ] {
            assert!(origin(invalid).is_err(), "accepted {invalid}");
        }
        assert_eq!(
            origin("https://example.test/").unwrap(),
            ("example.test".into(), 443)
        );
        assert_eq!(origin("https://[::1]:8443").unwrap(), ("::1".into(), 8443));
        assert_eq!(authority("::1", 8443), "[::1]:8443");
    }
}

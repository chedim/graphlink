//! TLS for the TCP transport (the feature `tls`): a server certificate the client checks against a file it names, and a maker of that certificate for a listener.
//! One self-signed certificate is the whole PKI: the server presents it, the client trusts exactly it (a pin by file), and its names (the subject alternative names) must cover the name the client dials.
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName};

fn provider() -> Arc<rustls::crypto::CryptoProvider> { Arc::new(rustls::crypto::ring::default_provider()) }

/// The server's side: its certificate chain and private key (PEM files).
#[derive(Clone)]
pub struct TlsServer(pub(crate) Arc<rustls::ServerConfig>);

impl TlsServer {
    pub fn from_files(cert_pem: &Path, key_pem: &Path) -> Result<TlsServer, String> {
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_pem).map_err(|e| format!("{}: {e}", cert_pem.display()))?
            .collect::<Result<_, _>>().map_err(|e| format!("{}: {e}", cert_pem.display()))?;
        let key = PrivateKeyDer::from_pem_file(key_pem).map_err(|e| format!("{}: {e}", key_pem.display()))?;
        let cfg = rustls::ServerConfig::builder_with_provider(provider()).with_safe_default_protocol_versions().map_err(|e| e.to_string())?
            .with_no_client_auth().with_single_cert(certs, key).map_err(|e| e.to_string())?;
        Ok(TlsServer(Arc::new(cfg)))
    }
}

/// The client's side: it trusts exactly the certificates of the file it names.
#[derive(Clone)]
pub struct TlsClient(pub(crate) Arc<rustls::ClientConfig>);

impl TlsClient {
    pub fn trusting(cert_pem: &Path) -> Result<TlsClient, String> {
        let mut roots = rustls::RootCertStore::empty();
        for c in CertificateDer::pem_file_iter(cert_pem).map_err(|e| format!("{}: {e}", cert_pem.display()))? {
            roots.add(c.map_err(|e| format!("{}: {e}", cert_pem.display()))?).map_err(|e| format!("{}: {e}", cert_pem.display()))?;
        }
        if roots.is_empty() { return Err(format!("{}: no certificate in the file", cert_pem.display())); }
        let cfg = rustls::ClientConfig::builder_with_provider(provider()).with_safe_default_protocol_versions().map_err(|e| e.to_string())?
            .with_root_certificates(roots).with_no_client_auth();
        Ok(TlsClient(Arc::new(cfg)))
    }
}

pub(crate) fn server_name(name: &str) -> Result<ServerName<'static>, String> { ServerName::try_from(name.to_string()).map_err(|e| format!("{name:?} is not a name to check a certificate for: {e}")) }

/// Make `cert.pem` and `key.pem` (mode 0600) in `dir`, a self-signed certificate for the names `sans` (DNS names and IP addresses), unless both are already there: a certificate that exists is never replaced.
pub fn make_certs(dir: &Path, sans: &[String]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    if cert.is_file() && key.is_file() { return Ok(()); }
    if sans.is_empty() { return Err("a certificate needs at least one name".into()); }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    params.subject_alt_names = sans.iter().map(|n| match n.parse::<std::net::IpAddr>() {
        Ok(ip) => Ok(rcgen::SanType::IpAddress(ip)),
        Err(_) => n.as_str().try_into().map(rcgen::SanType::DnsName).map_err(|e| format!("{n:?} is not a name for a certificate: {e}")),
    }).collect::<Result<_, _>>()?;
    let pair = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let made = params.self_signed(&pair).map_err(|e| e.to_string())?;
    let private = |path: &Path, text: &str| -> Result<(), String> {
        let _ = std::fs::remove_file(path);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
    };
    private(&key, &pair.serialize_pem())?;
    private(&cert, &made.pem())
}

/// The host part of an address (`127.0.0.1:4000` -> `127.0.0.1`, `[::1]:4000` -> `::1`, `example.org:4000` -> `example.org`): the name a client checks the certificate for unless it is told another.
pub fn host_of(addr: &str) -> &str { addr.rsplit_once(':').map_or(addr, |x| x.0).trim_start_matches('[').trim_end_matches(']') }

/// The names a listener's certificate covers: the address it listens on, `localhost` when that is a loopback address, and the extra names the owner gives.
pub fn sans_for(listening: std::net::IpAddr, extra: &[String]) -> Vec<String> {
    let mut v = vec![listening.to_string()];
    if listening.is_loopback() { v.push("localhost".into()); }
    v.extend(extra.iter().cloned());
    v
}

// Certificates for HTTPS tests: a test authority, and server certificates it issues,
// including deliberately bad ones (wrong name, expired).

use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::Arc;

pub struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
}

impl Ca {
    pub fn new(name: &str) -> Ca {
        let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        // A CA certificate says what its key is for (RFC 5280 section 4.2.1.3); strict
        // verifiers (Python 3.13's default) refuse one that does not.
        p.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
        p.distinguished_name.push(DnType::CommonName, name);
        Ca { issuer: CertifiedIssuer::self_signed(p, KeyPair::generate().unwrap()).unwrap() }
    }

    pub fn pem(&self) -> String {
        self.issuer.pem()
    }

    /// A server configuration with a certificate for `names`, valid between the years
    /// given (or rcgen's very wide default when None).
    pub fn server(&self, names: &[&str], years: Option<(i32, i32)>) -> Arc<rustls::ServerConfig> {
        let mut p = CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        // Strict verifiers (Python 3.13's default) require it (RFC 5280 section 4.2.1.1).
        p.use_authority_key_identifier_extension = true;
        if let Some((from, to)) = years {
            p.not_before = rcgen::date_time_ymd(from, 1, 1);
            p.not_after = rcgen::date_time_ymd(to, 1, 1);
        }
        let key = KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &self.issuer).unwrap();
        config(vec![cert.der().clone()], key)
    }
}

impl Ca {
    /// A certificate for `names` and its key, in PEM, for a server that reads files.
    pub fn server_pem(&self, names: &[&str]) -> (String, String) {
        let p = CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        let key = KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &self.issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    }
}

/// A certificate for `names` that signs itself, trusted by no one.
pub fn self_signed(names: &[&str]) -> Arc<rustls::ServerConfig> {
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap().self_signed(&key).unwrap();
    config(vec![cert.der().clone()], key)
}

fn config(chain: Vec<CertificateDer<'static>>, key: KeyPair) -> Arc<rustls::ServerConfig> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    Arc::new(cfg)
}

impl Ca {
    /// A certificate for `names` (valid in the years given, or rcgen's wide default) and
    /// its key, written as `chain.pem` and `key.pem` (mode 600) in `dir`, for a run that
    /// listens with `net.tls.accept`; returns their paths.
    pub fn listen_files(&self, dir: &std::path::Path, names: &[&str], years: Option<(i32, i32)>) -> (String, String) {
        use std::os::unix::fs::PermissionsExt;
        let mut p = CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        p.use_authority_key_identifier_extension = true;
        if let Some((from, to)) = years {
            p.not_before = rcgen::date_time_ymd(from, 1, 1);
            p.not_after = rcgen::date_time_ymd(to, 1, 1);
        }
        let key = KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &self.issuer).unwrap();
        let (c, k) = (dir.join("chain.pem"), dir.join("key.pem"));
        let _ = std::fs::remove_file(&k);
        std::fs::write(&c, cert.pem()).unwrap();
        std::fs::write(&k, key.serialize_pem()).unwrap();
        std::fs::set_permissions(&k, std::fs::Permissions::from_mode(0o600)).unwrap();
        (c.to_string_lossy().into_owned(), k.to_string_lossy().into_owned())
    }

    /// A client configuration trusting only this authority.
    pub fn client(&self) -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.issuer.der().clone()).unwrap();
        Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }
}

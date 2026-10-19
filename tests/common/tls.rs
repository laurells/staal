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
        if let Some((from, to)) = years {
            p.not_before = rcgen::date_time_ymd(from, 1, 1);
            p.not_after = rcgen::date_time_ymd(to, 1, 1);
        }
        let key = KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &self.issuer).unwrap();
        config(vec![cert.der().clone()], key)
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

// The addresses a network capability may connect to (RUNTIME.md section 12,
// "Addresses").
//
// `only` checks a target's host as text. What it is reached at is checked here: every
// address a name resolves to, or the address a target names, is checked immediately
// before connecting, and only addresses that pass are connected to. Nothing looks the
// name up again in between, so an answer that changes (DNS rebinding) changes nothing.
//
// Public addresses may be reached. Every other address (loopback, the private
// networks, link-local, shared, multicast, documentation, reserved, and IPv6's local
// ranges) may be reached only when the grant lists it, or a network holding it, in
// `private_addresses`. The addresses of cloud metadata services, which hand out the
// machine's credentials, are refused even then, unless the grant also says
// `"cloud_metadata": true`. An IPv6 address that carries an IPv4 one (mapped, or the
// NAT64 prefix) is judged by the IPv4 address it carries.

use serde_json::Value as Json;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// One network: an address and how many of its leading bits are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    /// `a.b.c.d`, `a.b.c.d/n`, an IPv6 address, or one with `/n`. The address must be
    /// the network's first (no bits set past the prefix), and an IPv4 address is
    /// written as one, not mapped into IPv6, so each network has one spelling.
    pub fn parse(s: &str) -> Result<Net, String> {
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a.parse().map_err(|_| format!("'{s}' is not an IP address or a network written address/prefix"))?;
        if let IpAddr::V6(v6) = addr
            && v6.to_ipv4_mapped().is_some()
        {
            return Err(format!("'{s}': write an IPv4 network as IPv4"));
        }
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            None => max,
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0')) => {
                p.parse::<u8>().ok().filter(|n| *n <= max).ok_or_else(|| format!("'{s}': a prefix is at most {max}"))?
            }
            Some(_) => return Err(format!("'{s}': a prefix is a number of bits, at most {max}")),
        };
        let n = Net { addr, prefix };
        if n.first() != addr {
            return Err(format!("'{s}' has bits set past its prefix: the network is {}", Net { addr: n.first(), prefix }.text()));
        }
        Ok(n)
    }

    fn bits(ip: IpAddr) -> (u128, u8) {
        match ip {
            IpAddr::V4(a) => (u32::from(a) as u128, 32),
            IpAddr::V6(a) => (u128::from(a), 128),
        }
    }

    fn mask(&self) -> u128 {
        let (_, width) = Net::bits(self.addr);
        if self.prefix == 0 { 0 } else { (u128::MAX << (width - self.prefix)) & (u128::MAX >> (128 - width as u32)) }
    }

    fn first(&self) -> IpAddr {
        let (b, _) = Net::bits(self.addr);
        let v = b & self.mask();
        match self.addr {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::from(v as u32)),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::from(v)),
        }
    }

    /// Whether `ip`, of the same family, is in this network.
    pub fn contains(&self, ip: IpAddr) -> bool {
        if self.addr.is_ipv4() != ip.is_ipv4() {
            return false;
        }
        let (b, _) = Net::bits(ip);
        let (n, _) = Net::bits(self.addr);
        b & self.mask() == n
    }

    /// Its canonical text: the address alone for a single one, else `address/prefix`.
    pub fn text(&self) -> String {
        let max = if self.addr.is_ipv4() { 32 } else { 128 };
        if self.prefix == max { self.addr.to_string() } else { format!("{}/{}", self.addr, self.prefix) }
    }
}

/// What an address is, for reaching it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Public,
    /// Not public: reached only when the grant lists it.
    Private,
    /// A cloud metadata service: reached only when the grant also allows those.
    Metadata,
}

/// The addresses of cloud metadata services: AWS's instance metadata (also Google
/// Cloud's, Azure's and others', at 169.254.169.254), its IPv6 address, the container
/// credential addresses of ECS and EKS Pod Identity, and Alibaba Cloud's. Each is also
/// a private address; listing it among them is not enough.
const METADATA: [IpAddr; 6] = [
    IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
    IpAddr::V4(Ipv4Addr::new(169, 254, 170, 2)),
    IpAddr::V4(Ipv4Addr::new(169, 254, 170, 23)),
    IpAddr::V4(Ipv4Addr::new(100, 100, 100, 200)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x23)),
];

/// IPv4 networks that are not public (the IANA IPv4 Special-Purpose Address Registry,
/// every block not marked globally reachable, whole, and multicast and the reserved
/// block above it).
const NOT_PUBLIC_V4: [(u32, u8); 15] = [
    (0x0000_0000, 8),  // 0.0.0.0/8: this network
    (0x0a00_0000, 8),  // 10.0.0.0/8: private
    (0x6440_0000, 10), // 100.64.0.0/10: shared address space (carrier NAT)
    (0x7f00_0000, 8),  // 127.0.0.0/8: loopback
    (0xa9fe_0000, 16), // 169.254.0.0/16: link-local
    (0xac10_0000, 12), // 172.16.0.0/12: private
    (0xc000_0000, 24), // 192.0.0.0/24: IETF protocol assignments
    (0xc000_0200, 24), // 192.0.2.0/24: documentation
    (0xc058_6300, 24), // 192.88.99.0/24: deprecated 6to4 relay anycast
    (0xc0a8_0000, 16), // 192.168.0.0/16: private
    (0xc612_0000, 15), // 198.18.0.0/15: benchmarking
    (0xc633_6400, 24), // 198.51.100.0/24: documentation
    (0xcb00_7100, 24), // 203.0.113.0/24: documentation
    (0xe000_0000, 4),  // 224.0.0.0/4: multicast
    (0xf000_0000, 4),  // 240.0.0.0/4: reserved, and 255.255.255.255
];

fn v4_public(a: Ipv4Addr) -> bool {
    let b = u32::from(a);
    !NOT_PUBLIC_V4.iter().any(|(net, p)| b & (u32::MAX << (32 - p)) == *net)
}

/// IPv6 networks inside 2000::/3 (the global unicast space) that are not public: IETF
/// protocol assignments (which hold Teredo), documentation, and 6to4, which tunnels to
/// IPv4 addresses that are not checked.
const NOT_PUBLIC_V6: [(u128, u8); 4] = [
    (0x2001_0000_0000_0000_0000_0000_0000_0000, 23), // 2001::/23
    (0x2001_0db8_0000_0000_0000_0000_0000_0000, 32), // 2001:db8::/32
    (0x2002_0000_0000_0000_0000_0000_0000_0000, 16), // 2002::/16
    (0x3fff_0000_0000_0000_0000_0000_0000_0000, 20), // 3fff::/20: documentation
];

/// The IPv4 address an IPv6 one carries, if it is one that does: mapped
/// (::ffff:a.b.c.d) or behind the well-known NAT64 prefix (64:ff9b::/96).
fn carried_v4(a: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = a.to_ipv4_mapped() {
        return Some(v4);
    }
    let b = u128::from(a);
    (b >> 32 == 0x0064_ff9b_0000_0000_0000_0000).then(|| Ipv4Addr::from(b as u32))
}

/// The address as it is judged: an IPv6 address carrying an IPv4 one is that IPv4 one.
fn judged(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(a) => carried_v4(a).map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// What `ip` is.
pub fn kind(ip: IpAddr) -> Kind {
    let ip = judged(ip);
    if METADATA.contains(&ip) {
        return Kind::Metadata;
    }
    let public = match ip {
        IpAddr::V4(a) => v4_public(a),
        IpAddr::V6(a) => {
            let b = u128::from(a);
            // Public IPv6 is the global unicast space, 2000::/3, less the blocks above.
            b >> 125 == 1 && !NOT_PUBLIC_V6.iter().any(|(net, p)| b & (u128::MAX << (128 - p)) == *net)
        }
    };
    if public { Kind::Public } else { Kind::Private }
}

/// Which addresses one grant may reach.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Networks beyond the public ones the grant may reach.
    pub private: Vec<Net>,
    /// Whether it may reach cloud metadata services.
    pub cloud_metadata: bool,
}

impl Policy {
    /// A grant's `private_addresses` (a list of addresses and networks) and
    /// `cloud_metadata` (true or false), both optional.
    pub fn from_grant(private: Option<&Json>, metadata: Option<&Json>, at: &str) -> Result<Policy, String> {
        let private = match private {
            None => Vec::new(),
            Some(Json::Array(items)) => {
                let mut nets = Vec::with_capacity(items.len());
                for (i, v) in items.iter().enumerate() {
                    let s = v.as_str().ok_or(format!("{at}.private_addresses[{i}]: expected an address or a network, as text"))?;
                    let n = Net::parse(s).map_err(|e| format!("{at}.private_addresses[{i}]: {e}"))?;
                    if nets.contains(&n) {
                        return Err(format!("{at}.private_addresses[{i}]: {} is listed twice", n.text()));
                    }
                    nets.push(n);
                }
                nets
            }
            Some(_) => return Err(format!("{at}.private_addresses: expected a list of addresses and networks")),
        };
        let cloud_metadata = match metadata {
            None => false,
            Some(Json::Bool(b)) => *b,
            Some(_) => return Err(format!("{at}.cloud_metadata: expected true or false")),
        };
        Ok(Policy { private, cloud_metadata })
    }

    /// Whether `ip` may be reached; if not, why.
    pub fn permits(&self, ip: IpAddr) -> Result<(), String> {
        let j = judged(ip);
        match kind(ip) {
            Kind::Public => Ok(()),
            Kind::Metadata if self.cloud_metadata => Ok(()),
            Kind::Metadata => Err(format!("{ip} is a cloud metadata service, which a grant reaches only with \"cloud_metadata\": true")),
            Kind::Private if self.private.iter().any(|n| n.contains(j)) => Ok(()),
            Kind::Private => Err(format!("{ip} is not a public address, and is not in this grant's private_addresses")),
        }
    }

    /// The addresses of `found` that may be reached, in their order; or, if none may,
    /// why the first could not.
    pub fn check(&self, found: impl IntoIterator<Item = SocketAddr>) -> Result<Vec<SocketAddr>, String> {
        let mut ok = Vec::new();
        let mut first_refusal = None;
        for a in found {
            match self.permits(a.ip()) {
                Ok(()) => ok.push(a),
                Err(e) => {
                    first_refusal.get_or_insert(e);
                }
            }
        }
        match (ok.is_empty(), first_refusal) {
            (false, _) => Ok(ok),
            (true, Some(e)) => Err(e),
            (true, None) => Err("no address".into()),
        }
    }
}

/// Why a request was not sent, found before connecting: a lookup that failed, or an
/// address refused. It is carried through an HTTP client that knows only I/O errors,
/// so that it can be told apart from every other failure.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn public_and_not() {
        for p in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "100.63.255.255", "100.128.0.0", "172.15.255.255", "172.32.0.0",
            "192.0.1.255", "192.0.3.0", "192.167.255.255", "192.169.0.0", "198.17.255.255", "198.20.0.0", "223.255.255.255",
            "2606:4700:4700::1111", "2a00:1450:4001::1", "2001:200::1", "64:ff9b::808:808", "::ffff:8.8.8.8"]
        {
            assert_eq!(kind(ip(p)), Kind::Public, "{p}");
        }
        for p in ["0.0.0.0", "0.1.2.3", "10.0.0.1", "10.255.255.255", "100.64.0.1", "100.127.255.255", "127.0.0.1", "127.255.255.254",
            "169.254.0.1", "172.16.0.1", "172.31.255.255", "192.0.0.8", "192.0.2.1", "192.88.99.1", "192.168.1.1", "198.18.0.1",
            "198.19.255.255", "198.51.100.7", "203.0.113.9", "224.0.0.1", "239.255.255.255", "240.0.0.1", "255.255.255.255",
            "::", "::1", "::2", "::ffff:127.0.0.1", "::ffff:10.1.2.3", "64:ff9b::a00:1", "64:ff9b:1::1", "100::1", "2001::1",
            "2001:0:4136:e378::1", "2001:db8::1", "2002:c000:0204::1", "3fff::1", "fc00::1", "fd12:3456::1", "fe80::1",
            "fec0::1", "ff02::1", "1000::1", "4000::1", "5f00::1"]
        {
            assert_eq!(kind(ip(p)), Kind::Private, "{p}");
        }
        for p in ["169.254.169.254", "169.254.170.2", "169.254.170.23", "100.100.100.200", "fd00:ec2::254", "fd00:ec2::23",
            "::ffff:169.254.169.254", "64:ff9b::a9fe:a9fe"]
        {
            assert_eq!(kind(ip(p)), Kind::Metadata, "{p}");
        }
    }

    #[test]
    fn networks_parse_to_one_spelling() {
        for (s, text) in [("10.0.0.0/8", "10.0.0.0/8"), ("127.0.0.1", "127.0.0.1"), ("127.0.0.1/32", "127.0.0.1"),
            ("fd00::/8", "fd00::/8"), ("::1", "::1"), ("0.0.0.0/0", "0.0.0.0/0"), ("::/0", "::/0")]
        {
            assert_eq!(Net::parse(s).unwrap().text(), text, "{s}");
        }
        for (s, why) in [("10.0.0.1/8", "bits set past its prefix: the network is 10.0.0.0/8"), ("10.0.0.0/33", "at most 32"),
            ("fd00::/129", "at most 128"), ("10.0.0.0/", "a number of bits"), ("10.0.0.0/08", "a number of bits"),
            ("10.0.0.0/+8", "a number of bits"), ("::ffff:10.0.0.1", "write an IPv4 network as IPv4"), ("localhost", "not an IP"),
            ("10.0.0", "not an IP"), (" 10.0.0.1", "not an IP")]
        {
            let e = Net::parse(s).unwrap_err();
            assert!(e.contains(why), "{s}: {e}");
        }
        let n = Net::parse("10.1.0.0/16").unwrap();
        assert!(n.contains(ip("10.1.255.255")) && n.contains(ip("10.1.0.0")));
        assert!(!n.contains(ip("10.2.0.0")) && !n.contains(ip("::ffff:10.1.0.1")));
        assert!(Net::parse("0.0.0.0/0").unwrap().contains(ip("10.0.0.1")));
        assert!(!Net::parse("0.0.0.0/0").unwrap().contains(ip("::1")));
    }

    #[test]
    fn a_grant_reaches_public_addresses_and_the_private_ones_it_lists() {
        let none = Policy::default();
        assert!(none.permits(ip("8.8.8.8")).is_ok());
        assert!(none.permits(ip("127.0.0.1")).unwrap_err().contains("not a public address"));
        let p = Policy::from_grant(Some(&serde_json::json!(["127.0.0.1", "10.1.0.0/16", "fd00::/8"])), None, "g").unwrap();
        assert!(p.permits(ip("127.0.0.1")).is_ok());
        assert!(p.permits(ip("::ffff:127.0.0.1")).is_ok(), "a mapped address is the IPv4 one");
        assert!(p.permits(ip("127.0.0.2")).is_err());
        assert!(p.permits(ip("10.1.2.3")).is_ok());
        assert!(p.permits(ip("10.2.0.1")).is_err());
        assert!(p.permits(ip("fd12::1")).is_ok());
        // A metadata service is refused though its network is listed...
        let p = Policy::from_grant(Some(&serde_json::json!(["169.254.0.0/16", "fd00::/8"])), None, "g").unwrap();
        assert!(p.permits(ip("169.254.1.1")).is_ok());
        for m in ["169.254.169.254", "fd00:ec2::254", "64:ff9b::a9fe:a9fe"] {
            assert!(p.permits(ip(m)).unwrap_err().contains("cloud metadata"), "{m}");
        }
        // ...and reached only when the grant says so.
        let p = Policy::from_grant(None, Some(&serde_json::json!(true)), "g").unwrap();
        assert!(p.permits(ip("169.254.169.254")).is_ok());
        assert!(p.permits(ip("169.254.1.1")).is_err(), "cloud_metadata allows the services, not their network");
        for (priv_, meta, why) in [
            (serde_json::json!("10.0.0.0/8"), serde_json::json!(false), "expected a list"),
            (serde_json::json!([10]), serde_json::json!(false), "as text"),
            (serde_json::json!(["10.0.0.0/8", "10.0.0.0/8"]), serde_json::json!(false), "listed twice"),
            (serde_json::json!([]), serde_json::json!("yes"), "expected true or false"),
        ] {
            let e = Policy::from_grant(Some(&priv_), Some(&meta), "g").unwrap_err();
            assert!(e.contains(why), "{e}");
        }
    }

    #[test]
    fn only_the_addresses_that_pass_are_kept() {
        let a = |s: &str| -> SocketAddr { s.parse().unwrap() };
        let p = Policy::default();
        assert_eq!(p.check([a("10.0.0.1:443"), a("8.8.8.8:443"), a("[::1]:443")]).unwrap(), vec![a("8.8.8.8:443")]);
        assert!(p.check([a("10.0.0.1:443"), a("127.0.0.1:443")]).unwrap_err().contains("10.0.0.1"));
    }
}
